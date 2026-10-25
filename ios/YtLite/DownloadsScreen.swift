import SwiftUI

/// Pushed from You: what's downloading, videos you downloaded, smart
/// downloads, and the space they take.
struct DownloadsScreen: View {
    @State private var downloads = DownloadManager.shared
    @State private var items: [Download] = []
    @State private var storage: OfflineStorage?
    @State private var loaded = false

    var body: some View {
        let pending = items.filter { $0.state != .done }
        let saved = items.filter { $0.state == .done && !$0.smart }
        let smart = items.filter { $0.state == .done && $0.smart }
        List {
            if loaded && items.isEmpty {
                EmptyState(
                    icon: "arrow.down.circle",
                    title: "No downloads",
                    message: "Long-press a video and choose Download to watch it offline. "
                        + "Smart downloads (in Settings) save new videos for you while your iPhone charges."
                )
                .listRowBackground(Color.clear)
            }
            if !pending.isEmpty {
                Section("Downloading") {
                    ForEach(pending, id: \.video.id) { DownloadItem(download: $0) }
                }
            }
            if !saved.isEmpty {
                Section("Downloaded") {
                    ForEach(saved, id: \.video.id) { DownloadItem(download: $0) }
                }
            }
            if !smart.isEmpty {
                Section {
                    ForEach(smart, id: \.video.id) { DownloadItem(download: $0) }
                } header: {
                    Text("Smart downloads")
                } footer: {
                    Text("Picked from your subscriptions and For you. Each is replaced once you've watched it. Long-press one to keep it.")
                }
            }
            if let storage, !items.isEmpty {
                Section("Storage") {
                    LabeledContent("Downloaded", value: megabytes(storage.savedBytes))
                    if downloads.smartEnabled || storage.smartBytes > 0 {
                        LabeledContent(
                            "Smart downloads",
                            value: "\(megabytes(storage.smartBytes)) of \(downloads.smartGigabytes) GB"
                        )
                    }
                    if let free = storage.freeBytes {
                        LabeledContent("Free on this device", value: megabytes(free))
                    }
                }
            }
        }
        .navigationTitle("Downloads")
        .task(id: downloads.listVersion) { await reload() }
        .refreshable { await reload() }
    }

    private func reload() async {
        if let fresh = try? await downloads.downloads() { items = fresh }
        storage = await downloads.storage()
        loaded = true
    }
}

/// A download in the list: tap to play; swipe to delete or cancel.
private struct DownloadItem: View {
    @Environment(AppModel.self) private var model
    @State private var downloads = DownloadManager.shared
    let download: Download

    private var video: Video { download.video }

    var body: some View {
        HStack(spacing: 12) {
            Thumbnail(url: video.thumbnailURL)
                .frame(width: 112, height: 63)
                .clipShape(RoundedRectangle(cornerRadius: 8))
                .overlay(alignment: .bottom) {
                    if let fraction = video.progress ?? (video.watched ? 1 : nil) {
                        ProgressBar(fraction: fraction, cornerRadius: 8)
                    }
                }
            VStack(alignment: .leading, spacing: 3) {
                Text(video.title)
                    .font(.subheadline.weight(.semibold))
                    .lineLimit(2)
                Text(detail)
                    .font(.caption)
                    .foregroundStyle(download.state == .failed ? .red : .secondary)
                    .lineLimit(2)
                if let fraction {
                    ProgressView(value: fraction).tint(.red)
                }
            }
        }
        .opacity(video.watched && download.state == .done ? 0.55 : 1)
        .contentShape(Rectangle())
        .onTapGesture {
            if download.state == .done { model.play(video) }
        }
        .swipeActions {
            Button(role: .destructive) {
                downloads.remove(video)
            } label: {
                Label(download.state == .done ? "Delete" : "Cancel", systemImage: "trash")
            }
        }
        .contextMenu {
            if download.state == .done {
                Button {
                    model.play(video, audioOnly: false)
                } label: {
                    Label("Watch", systemImage: "play.rectangle")
                }
                Button {
                    model.play(video, audioOnly: true)
                } label: {
                    Label("Listen (audio only)", systemImage: "headphones")
                }
            }
            if download.smart {
                Button {
                    downloads.keep(video)
                } label: {
                    Label("Keep downloaded", systemImage: "pin")
                }
            }
            if download.state == .failed {
                Button {
                    downloads.download(video, audioOnly: download.audioOnly)
                } label: {
                    Label("Try again", systemImage: "arrow.clockwise")
                }
            }
            Button(role: .destructive) {
                downloads.remove(video)
            } label: {
                Label(download.state == .done ? "Delete download" : "Cancel download", systemImage: "trash")
            }
        }
    }

    /// The active download's progress, from the manager (it changes twice a
    /// second; the list itself reloads only when downloads come and go).
    private var active: DownloadProgress? {
        downloads.progress.flatMap { $0.videoId == video.id ? $0 : nil }
    }

    private var fraction: Double? {
        guard download.state != .done, download.state != .failed else { return nil }
        guard let active, active.totalBytes > 0 else { return download.state == .downloading ? 0 : nil }
        return Double(active.doneBytes) / Double(active.totalBytes)
    }

    private var detail: String {
        let audio = download.audioOnly ? " · Audio only" : ""
        switch download.state {
        case .done:
            return "\(video.channel) · \(megabytes(download.bytes))\(audio)"
        case .failed:
            return download.error ?? "Couldn't download"
        case .downloading, .queued:
            if let active, active.totalBytes > 0 {
                return "\(megabytes(active.doneBytes)) of \(megabytes(active.totalBytes))\(audio)"
            }
            if active != nil { return "Starting…" }
            return download.error.map { "Waiting · \($0)" } ?? "Waiting\(audio)"
        }
    }
}
