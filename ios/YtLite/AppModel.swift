import Foundation
import Observation

/// A video ready for the player.
struct PlayItem: Identifiable {
    let id: String
    let url: URL
    let video: Video
    let maxHeight: Int
    /// Resume point (seconds), or 0.
    let start: Double
}

/// Owns the Rust core (`YtLite`). Every core call is blocking, so it runs on
/// a background task via `background { }`. Shared by the phone UI and CarPlay.
@Observable
@MainActor
final class AppModel {
    static let shared = AppModel()

    private(set) var core: YtLite?
    var signedIn = false
    var signingIn = false
    var resolving = false
    var errorMessage: String?
    /// Bumped when watched state or progress changes so screens can reload.
    var watchedVersion = 0
    /// Set if the iOS Keychain can't store the sign-in token.
    var credentialProblem: String?

    var maxHeight: Int = UserDefaults.standard.object(forKey: "maxHeight") as? Int ?? 1080 {
        didSet { UserDefaults.standard.set(maxHeight, forKey: "maxHeight") }
    }

    /// Keep playing (as audio) when the phone locks or the app is in the background.
    var backgroundPlayback: Bool = UserDefaults.standard.object(forKey: "backgroundPlayback") as? Bool ?? true {
        didSet { UserDefaults.standard.set(backgroundPlayback, forKey: "backgroundPlayback") }
    }

    /// Play only the audio track by default (podcasts).
    var audioOnly: Bool = UserDefaults.standard.bool(forKey: "audioOnly") {
        didSet { UserDefaults.standard.set(audioOnly, forKey: "audioOnly") }
    }

    var hideWatched: Bool = UserDefaults.standard.bool(forKey: "hideWatched") {
        didSet {
            UserDefaults.standard.set(hideWatched, forKey: "hideWatched")
            watchedVersion += 1
        }
    }

    init() {
        do {
            let root = try FileManager.default.url(
                for: .applicationSupportDirectory, in: .userDomainMask, appropriateFor: nil, create: true
            ).appendingPathComponent("yt-lite", isDirectory: true)
            try FileManager.default.createDirectory(at: root, withIntermediateDirectories: true)
            let core = try YtLite(
                rootDir: root.path,
                clientId: Secrets.googleClientId,
                clientSecret: Secrets.googleClientSecret
            )
            self.core = core
            Task { await checkSignIn() }
        } catch {
            errorMessage = "Couldn't start: \(describe(error))"
        }
    }

    /// Also run when the app becomes active: if CarPlay launched it while the
    /// phone was locked, the Keychain couldn't be read then.
    func checkSignIn() async {
        guard let core, !signingIn else { return }
        signedIn = (try? await background { core.isSignedIn() }) ?? false
        credentialProblem = try? await background { core.credentialStoreProblem() }
    }

    var hasGoogleClient: Bool { core?.hasGoogleClient() ?? false }

    func videos(_ feed: Feed) async throws -> [Video] {
        guard let core else { return [] }
        let hide = hideWatched
        return try await background { try core.videos(feed: feed, hideWatched: hide) }
    }

    /// Videos played or marked watched, most recent first.
    func history() async throws -> [Video] {
        guard let core else { return [] }
        return try await background { try core.history() }
    }

    /// Also unmarks it watched and forgets its resume point.
    func removeFromHistory(_ video: Video) {
        guard let core else { return }
        Task {
            try? await background { try core.removeFromHistory(id: video.id) }
            watchedVersion += 1
        }
    }

    func clearHistory() {
        guard let core else { return }
        Task {
            try? await background { try core.clearHistory() }
            watchedVersion += 1
        }
    }

    /// `videos` with their watched state and progress re-read from the
    /// cache, in the same order (Explore and Search keep their own lists).
    func refreshed(_ videos: [Video], from feed: Feed) async -> [Video] {
        guard let core, !videos.isEmpty,
              let fresh = try? await background({ try core.videos(feed: feed, hideWatched: false) })
        else { return videos }
        let byId = Dictionary(fresh.map { ($0.id, $0) }, uniquingKeysWith: { first, _ in first })
        return videos.map { byId[$0.id] ?? $0 }
    }

    func refresh(_ feed: Feed) async throws {
        guard let core else { return }
        _ = try await background { try core.refresh(feed: feed) }
    }

    func search(_ query: String) async throws -> [Video] {
        guard let core else { return [] }
        return try await background { try core.search(query: query) }
    }

    func explore(topic: Int) async throws -> [Video] {
        guard let core else { return [] }
        return try await background { try core.explore(topic: UInt32(topic)) }
    }

    var exploreTopics: [String] { core?.exploreTopics() ?? [] }

    func setWatched(_ video: Video, _ watched: Bool) {
        guard let core else { return }
        Task {
            try? await background { try core.setWatched(id: video.id, watched: watched) }
            watchedVersion += 1
        }
    }

    /// `audioOnly: nil` uses the Settings default. Resumes where the video
    /// was stopped unless `fromStart`.
    func play(_ video: Video, audioOnly: Bool? = nil, fromStart: Bool = false) {
        let wantAudio = audioOnly ?? self.audioOnly
        Task {
            do {
                try await start(video, audioOnly: wantAudio, fromStart: fromStart)
            } catch {
                errorMessage = describe(error)
            }
        }
    }

    /// Resolves and starts playback. Throws rather than showing the alert,
    /// so CarPlay can show its own. The video counts as watched once it has
    /// played to the end (`PlaybackProgress`).
    func start(_ video: Video, audioOnly: Bool, fromStart: Bool = false) async throws {
        guard let core, !resolving else { return }
        resolving = true
        defer { resolving = false }
        let maxHeight = maxHeight
        let playable = try await background {
            try core.play(id: video.id, maxHeight: UInt32(maxHeight), audioOnly: audioOnly, fromStart: fromStart)
        }
        guard let url = URL(string: playable.url) else { return }
        if playable.audioOnly {
            PlayerPresenter.shared.stop()
            AudioPlayer.shared.play(url: url, video: video, start: playable.startSecs)
        } else {
            AudioPlayer.shared.stop()
            PlayerPresenter.shared.present(
                PlayItem(id: video.id, url: url, video: video, maxHeight: maxHeight, start: playable.startSecs),
                backgroundPlayback: backgroundPlayback
            )
        }
        // The core marked it played: History moves it to the top.
        watchedVersion += 1
    }

    /// Saves a resume point, or marks the video watched once it's finished.
    /// `refresh` reloads the lists so their progress bars catch up.
    func saveProgress(_ video: Video, position: Double, duration: Double?, refresh: Bool) {
        guard let core else { return }
        Task {
            try? await background {
                try core.saveProgress(id: video.id, positionSecs: position, durationSecs: duration)
            }
            if refresh { watchedVersion += 1 }
        }
    }

    func signIn() {
        guard let core, !signingIn else { return }
        if let credentialProblem {
            errorMessage = "Can't save the sign-in: \(credentialProblem)"
            return
        }
        signingIn = true
        Task {
            defer { signingIn = false }
            do {
                let pending = try await background { try core.startSignIn() }
                guard let url = URL(string: pending.url()) else { return }
                let session = WebAuth.start(url: url) { pending.cancel() }
                defer { session.cancel() }
                try await background { try core.finishSignIn(signIn: pending) }
                signedIn = true
            } catch let error as FfiError {
                if case .Failed(let message) = error, message.contains("cancelled") { return }
                errorMessage = describe(error)
            } catch {
                errorMessage = describe(error)
            }
        }
    }

    func signOut() {
        guard let core else { return }
        Task {
            try? await background { try core.signOut() }
            signedIn = false
        }
    }
}

/// Runs blocking work off the main actor.
func background<T: Sendable>(_ work: @escaping @Sendable () throws -> T) async throws -> T {
    try await Task.detached(priority: .userInitiated) { try work() }.value
}

/// A readable message for errors from the Rust core (UniFFI's default
/// description is the debug form).
func describe(_ error: Error) -> String {
    switch error as? FfiError {
    case .NotSignedIn: "Sign in with Google in Settings to load your subscriptions."
    case .Failed(let message): message
    case nil: error.localizedDescription
    }
}

extension Video: Identifiable {}

extension Video {
    var thumbnailURL: URL? { URL(string: "https://i.ytimg.com/vi/\(id)/mqdefault.jpg") }

    var durationText: String? {
        if live { return "LIVE" }
        guard let secs = durationSecs, secs > 0 else { return nil }
        let h = secs / 3600, m = (secs % 3600) / 60, s = secs % 60
        return h > 0 ? String(format: "%d:%02d:%02d", h, m, s) : String(format: "%d:%02d", m, s)
    }

    var ageText: String { Self.relative(published) }

    /// "watched 2 hours ago" (History).
    var watchedText: String? { lastPlayed.map { "watched \(Self.relative($0))" } }

    private static func relative(_ unix: Int64) -> String {
        let formatter = RelativeDateTimeFormatter()
        formatter.unitsStyle = .full
        return formatter.localizedString(for: Date(timeIntervalSince1970: TimeInterval(unix)), relativeTo: .now)
    }
}
