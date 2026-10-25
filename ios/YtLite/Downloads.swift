import BackgroundTasks
import Network
import Observation
import UIKit

/// Downloads for offline playback. The Rust core does the work
/// (`runDownloads`, blocking); this decides when it runs:
///
/// - Downloads you ask for start at once. On iOS 26 they keep going after
///   you leave the app, as a continued-processing task with the system's
///   progress UI. Before that, iOS gives the app a few seconds; the rest
///   continues when you're back, or in the background task below.
/// - Smart downloads keep what you're likely to watch next (subscriptions,
///   then For you) up to a storage budget. They run in a `BGProcessingTask`
///   that iOS starts while the phone is charging and online, fetch only on
///   Wi-Fi (not a hotspot, not Low Data Mode), and stop when the charger or
///   Wi-Fi goes away. "Update now" in Settings runs them on Wi-Fi at once.
@Observable
@MainActor
final class DownloadManager {
    static let shared = DownloadManager()

    nonisolated static let backgroundTask = "\(Bundle.main.bundleIdentifier ?? "local.ytlite.ios").downloads"
    nonisolated private static let continuedPrefix = "\(Bundle.main.bundleIdentifier ?? "local.ytlite.ios").download"

    /// Bumped when downloads come, go, finish or fail (not on progress); the
    /// Downloads screen reloads on it.
    private(set) var listVersion = 0
    /// The download in progress.
    private(set) var progress: DownloadProgress?
    /// Downloads started from the app are running (in the app, or as a
    /// continued-processing task), not the overnight background task.
    private(set) var running = false

    /// Video quality of new downloads (H.264 tops out at 1080p).
    var quality: Int = UserDefaults.standard.object(forKey: "downloadQuality") as? Int ?? 720 {
        didSet { UserDefaults.standard.set(quality, forKey: "downloadQuality") }
    }

    var smartEnabled: Bool = UserDefaults.standard.object(forKey: "smartDownloads") as? Bool ?? true {
        didSet {
            UserDefaults.standard.set(smartEnabled, forKey: "smartDownloads")
            if !smartEnabled, let core {
                Task {
                    try? await background { try core.clearSmartDownloads() }
                    changed(progressOnly: false)
                }
            }
            scheduleBackgroundWork()
        }
    }

    /// Storage for smart downloads, in GB.
    var smartGigabytes: Int = UserDefaults.standard.object(forKey: "smartGigabytes") as? Int ?? 5 {
        didSet { UserDefaults.standard.set(smartGigabytes, forKey: "smartGigabytes") }
    }

    @ObservationIgnored private var core: YtLite? { AppModel.shared.core }
    @ObservationIgnored private var inApp: Task<Void, Never>?
    /// A continued-processing task was submitted and hasn't finished.
    @ObservationIgnored private var continuedSubmitted = false
    /// The iOS 26 continued-processing task running downloads, if any.
    @ObservationIgnored private var continued: AnyObject?
    /// Downloads finished during the continued task, for its overall progress.
    @ObservationIgnored private var continuedDone = 0
    @ObservationIgnored private var lastVideoId: String?

    private init() {}

    /// From the app's init: background tasks must be registered before
    /// launch finishes. Also keeps downloads out of iCloud backups.
    func setUp() {
        BGTaskScheduler.shared.register(forTaskWithIdentifier: Self.backgroundTask, using: nil) { task in
            guard let task = task as? BGProcessingTask else { return }
            Task { @MainActor in DownloadManager.shared.runInBackground(task) }
        }
        guard let core else { return }
        core.setDownloadListener(listener: Listener())
        var folder = URL(fileURLWithPath: core.downloadsDir(), isDirectory: true)
        try? FileManager.default.createDirectory(at: folder, withIntermediateDirectories: true)
        var values = URLResourceValues()
        values.isExcludedFromBackup = true
        try? folder.setResourceValues(values)
    }

    var smartPolicy: SmartDownloads {
        SmartDownloads(
            budgetBytes: UInt64(smartGigabytes) << 30,
            maxHeight: UInt32(quality),
            // Listeners get the sound only: far smaller.
            audioOnly: AppModel.shared.audioOnly
        )
    }

    // MARK: Actions

    func download(_ video: Video, audioOnly: Bool = false) {
        guard let core else { return }
        let quality = UInt32(quality)
        Task {
            do {
                try await background { try core.download(id: video.id, maxHeight: quality, audioOnly: audioOnly) }
                start(smart: false)
            } catch {
                AppModel.shared.errorMessage = describe(error)
            }
        }
    }

    /// Keeps a smart download from being replaced. (Asking for the sound
    /// only keeps whatever it has rather than fetching the picture too.)
    func keep(_ video: Video) {
        download(video, audioOnly: true)
    }

    /// Deletes a download or cancels it.
    func remove(_ video: Video) {
        guard let core else { return }
        Task {
            try? await background { try core.removeDownload(id: video.id) }
        }
    }

    /// Smart downloads now, on Wi-Fi (no charger needed): Settings' "Update now".
    func updateSmartNow() async {
        guard await Conditions.current().wifi else {
            AppModel.shared.errorMessage = "Connect to Wi-Fi to update smart downloads."
            return
        }
        start(smart: true)
    }

    func downloads() async throws -> [Download] {
        guard let core else { return [] }
        return try await background { try core.downloads() }
    }

    func storage() async -> OfflineStorage? {
        guard let core else { return nil }
        return try? await background { try core.offlineStorage() }
    }

    /// Continues downloads you asked for (the app is back, or launched).
    func resume() {
        guard let core, !running else { return }
        Task {
            if (try? await background({ try core.downloadsWaiting() })) ?? 0 > 0 { start(smart: false) }
        }
    }

    // MARK: Running

    /// Runs downloads now: as a continued-processing task where iOS has
    /// them, else in the app.
    private func start(smart: Bool) {
        if #available(iOS 26, *), submitContinued(smart: smart) { return }
        runInApp(smart: smart)
    }

    private func runInApp(smart: Bool) {
        guard let core else { return }
        // A run picks up anything asked for meanwhile. Smart ones only start
        // from Settings, so this one stays as it is.
        if inApp != nil { return }
        running = true
        let policy = smart ? smartPolicy : nil
        inApp = Task {
            let cancel = CancelToken()
            let watcher = smart ? Conditions.Watcher(needsPower: false) { cancel.cancel() } : nil
            // A few more seconds after the app leaves the screen.
            let grace = UIApplication.shared.beginBackgroundTask(withName: "Downloads") { cancel.cancel() }
            _ = try? await longRunning { try core.runDownloads(smart: policy, cancel: cancel) }
            watcher?.stop()
            UIApplication.shared.endBackgroundTask(grace)
            inApp = nil
            running = continuedSubmitted
            scheduleBackgroundWork()
        }
    }

    @available(iOS 26, *)
    private func submitContinued(smart: Bool) -> Bool {
        if continuedSubmitted { return true }
        // Each request gets its own identifier (Info.plist allows them with
        // a wildcard), registered just before it's submitted.
        let identifier = "\(Self.continuedPrefix).\(UUID().uuidString.prefix(8))"
        let registered = BGTaskScheduler.shared.register(forTaskWithIdentifier: identifier, using: nil) { task in
            guard let task = task as? BGContinuedProcessingTask else { return }
            Task { @MainActor in DownloadManager.shared.runContinued(task, smart: smart) }
        }
        guard registered else { return false }
        let request = BGContinuedProcessingTaskRequest(
            identifier: identifier,
            title: smart ? "Updating smart downloads" : "Downloading videos",
            subtitle: "Starting…"
        )
        // If it can't run right now, download in the app instead.
        request.strategy = .fail
        do {
            try BGTaskScheduler.shared.submit(request)
            continuedSubmitted = true
            running = true
            return true
        } catch {
            return false
        }
    }

    @available(iOS 26, *)
    private func runContinued(_ task: BGContinuedProcessingTask, smart: Bool) {
        guard let core, inApp == nil else {
            continuedSubmitted = false
            task.setTaskCompleted(success: true)
            return
        }
        continued = task
        continuedDone = 0
        task.progress.totalUnitCount = 100
        let cancel = CancelToken()
        task.expirationHandler = { cancel.cancel() }
        let watcher = smart ? Conditions.Watcher(needsPower: false) { cancel.cancel() } : nil
        let policy = smart ? smartPolicy : nil
        Task {
            let result = try? await longRunning { try core.runDownloads(smart: policy, cancel: cancel) }
            watcher?.stop()
            continued = nil
            continuedSubmitted = false
            running = inApp != nil
            task.setTaskCompleted(success: result?.outcome == .done)
            scheduleBackgroundWork()
        }
    }

    /// The background task: downloads you asked for, and smart downloads if
    /// the phone is on Wi-Fi and charging.
    private func runInBackground(_ task: BGProcessingTask) {
        guard let core else {
            task.setTaskCompleted(success: false)
            return
        }
        let cancel = CancelToken()
        task.expirationHandler = { cancel.cancel() }
        let smartEnabled = smartEnabled
        let policy = smartPolicy
        Task {
            let now = await Conditions.current()
            let smart = smartEnabled && now.wifi && now.charging
            let watcher = smart ? Conditions.Watcher(needsPower: true) { cancel.cancel() } : nil
            let result = try? await longRunning {
                try core.runDownloads(smart: smart ? policy : nil, cancel: cancel)
            }
            watcher?.stop()
            task.setTaskCompleted(success: result.map { $0.outcome != .offline } ?? false)
            scheduleBackgroundWork()
        }
    }

    /// Asks iOS for the background task: soon (on any connection) if
    /// downloads you asked for are waiting, otherwise for smart downloads
    /// while charging. Called whenever the app leaves the screen.
    func scheduleBackgroundWork() {
        guard let core else { return }
        let smart = smartEnabled
        Task {
            let waiting = (try? await background { try core.downloadsWaiting() }) ?? 0
            guard smart || waiting > 0 else {
                BGTaskScheduler.shared.cancel(taskRequestWithIdentifier: Self.backgroundTask)
                return
            }
            let request = BGProcessingTaskRequest(identifier: Self.backgroundTask)
            request.requiresNetworkConnectivity = true
            request.requiresExternalPower = waiting == 0
            request.earliestBeginDate = Date(timeIntervalSinceNow: waiting > 0 ? 60 : 30 * 60)
            do {
                try BGTaskScheduler.shared.submit(request)
            } catch {
                // The Simulator has no background tasks.
                print("Scheduling downloads: \(error)")
            }
        }
    }

    // MARK: Changes

    fileprivate func changed(progressOnly: Bool) {
        guard let core else { return }
        Task {
            let now = try? await background { core.downloadProgress() }
            progress = now
            if !progressOnly { listVersion += 1 }
            if now?.videoId != lastVideoId {
                // One finished (or failed): its card's badge changes.
                if lastVideoId != nil { continuedDone += 1 }
                lastVideoId = now?.videoId
            }
            if !progressOnly { AppModel.shared.watchedVersion += 1 }
            if #available(iOS 26, *), let task = continued as? BGContinuedProcessingTask {
                report(now, to: task)
            }
        }
    }

    @available(iOS 26, *)
    private func report(_ now: DownloadProgress?, to task: BGContinuedProcessingTask) {
        guard let now else { return }
        let fraction = now.totalBytes > 0 ? Double(now.doneBytes) / Double(now.totalBytes) : 0
        let count = continuedDone + max(Int(now.waiting), 1)
        task.progress.totalUnitCount = Int64(count * 100)
        task.progress.completedUnitCount = Int64(continuedDone * 100) + Int64(fraction * 100)
        let size = now.totalBytes > 0
            ? "\(megabytes(now.doneBytes)) of \(megabytes(now.totalBytes))"
            : "Starting…"
        let more = now.waiting > 1 ? " · \(now.waiting - 1) more" : ""
        task.updateTitle(now.title.isEmpty ? "Downloading videos" : now.title, subtitle: size + more)
    }
}

/// Hears from the core (on its download thread).
private final class Listener: DownloadListener {
    func downloadsChanged(progressOnly: Bool) {
        Task { @MainActor in DownloadManager.shared.changed(progressOnly: progressOnly) }
    }
}

/// Wi-Fi and power, for smart downloads.
enum Conditions {
    struct Now {
        /// Online, and not on cellular, a hotspot or in Low Data Mode.
        let wifi: Bool
        let charging: Bool
    }

    static func current() async -> Now {
        let path = await withCheckedContinuation { continuation in
            let monitor = NWPathMonitor()
            monitor.pathUpdateHandler = { path in
                monitor.cancel()
                continuation.resume(returning: path)
            }
            monitor.start(queue: .global(qos: .utility))
        }
        return Now(wifi: isWifi(path), charging: await isCharging())
    }

    static func isWifi(_ path: NWPath) -> Bool {
        path.status == .satisfied && !path.isExpensive && !path.isConstrained
    }

    @MainActor
    static func isCharging() -> Bool {
        UIDevice.current.isBatteryMonitoringEnabled = true
        // Unknown in the Simulator; iOS only starts the task on power anyway.
        return UIDevice.current.batteryState != .unplugged
    }

    /// Calls `lost` once if Wi-Fi (or, with `needsPower`, the charger) goes away.
    @MainActor
    final class Watcher {
        private let monitor = NWPathMonitor()
        private var battery: NSObjectProtocol?
        private var fired = false
        private let lost: @Sendable () -> Void

        init(needsPower: Bool, lost: @escaping @Sendable () -> Void) {
            self.lost = lost
            monitor.pathUpdateHandler = { [weak self] path in
                guard !Conditions.isWifi(path) else { return }
                Task { @MainActor in self?.fire() }
            }
            monitor.start(queue: .global(qos: .utility))
            if needsPower {
                UIDevice.current.isBatteryMonitoringEnabled = true
                battery = NotificationCenter.default.addObserver(
                    forName: UIDevice.batteryStateDidChangeNotification, object: nil, queue: .main
                ) { [weak self] _ in
                    MainActor.assumeIsolated {
                        if UIDevice.current.batteryState == .unplugged { self?.fire() }
                    }
                }
            }
        }

        func stop() {
            monitor.cancel()
            if let battery { NotificationCenter.default.removeObserver(battery) }
            battery = nil
        }

        private func fire() {
            guard !fired else { return }
            fired = true
            lost()
        }
    }
}

/// Runs minutes of blocking work (downloads) on a GCD thread rather than
/// tying up one of Swift concurrency's few threads.
func longRunning<T: Sendable>(_ work: @escaping @Sendable () throws -> T) async throws -> T {
    try await withCheckedThrowingContinuation { continuation in
        DispatchQueue.global(qos: .utility).async {
            continuation.resume(with: Result { try work() })
        }
    }
}

private let byteFormatter: ByteCountFormatter = {
    let formatter = ByteCountFormatter()
    formatter.countStyle = .file
    formatter.allowedUnits = [.useMB, .useGB]
    formatter.allowsNonnumericFormatting = false
    return formatter
}()

/// "43.7 MB", "1.2 GB", "0 MB".
func megabytes(_ bytes: UInt64) -> String {
    byteFormatter.string(fromByteCount: Int64(bytes))
}
