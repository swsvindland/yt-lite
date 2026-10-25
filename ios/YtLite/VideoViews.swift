import SwiftUI

/// Adaptive grid of video cards: one column on iPhone, more on iPad.
/// `inHistory`: cards say when they were watched and can be removed.
struct VideoGrid: View {
    @Environment(AppModel.self) private var model
    let videos: [Video]
    var inHistory = false

    var body: some View {
        LazyVGrid(columns: [GridItem(.adaptive(minimum: 300), spacing: 16)], spacing: 20) {
            ForEach(videos) { video in
                VideoCard(video: video, inHistory: inHistory)
                    .onTapGesture { model.play(video) }
                    .contextMenu {
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
                        if video.progress != nil {
                            Button {
                                model.play(video, fromStart: true)
                            } label: {
                                Label("Start over", systemImage: "arrow.counterclockwise")
                            }
                        }
                        Button {
                            model.setWatched(video, !video.watched)
                        } label: {
                            Label(
                                video.watched ? "Mark as unwatched" : "Mark as watched",
                                systemImage: video.watched ? "eye.slash" : "eye"
                            )
                        }
                        ShareLink(item: URL(string: "https://www.youtube.com/watch?v=\(video.id)")!)
                        if inHistory {
                            Button(role: .destructive) {
                                model.removeFromHistory(video)
                            } label: {
                                Label("Remove from history", systemImage: "minus.circle")
                            }
                        }
                    }
            }
        }
        .padding(.horizontal)
    }
}

struct VideoCard: View {
    let video: Video
    var inHistory = false

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            ZStack(alignment: .bottomTrailing) {
                Thumbnail(url: video.thumbnailURL)
                    .aspectRatio(16 / 9, contentMode: .fit)
                    .clipShape(RoundedRectangle(cornerRadius: 12))

                if let duration = video.durationText {
                    Text(duration)
                        .font(.caption2.weight(.semibold))
                        .padding(.horizontal, 6)
                        .padding(.vertical, 2)
                        .background(video.live ? Color.red : Color.black.opacity(0.78), in: RoundedRectangle(cornerRadius: 5))
                        .foregroundStyle(.white)
                        .padding(8)
                }
                if let fraction = video.progress ?? (video.watched ? 1 : nil) {
                    ProgressBar(fraction: fraction)
                }
            }
            VStack(alignment: .leading, spacing: 3) {
                Text(video.title)
                    .font(.subheadline.weight(.semibold))
                    .lineLimit(2)
                Text("\(video.channel) · \((inHistory ? video.watchedText : nil) ?? video.ageText)")
                    .font(.caption)
                    .foregroundStyle(.secondary)
                    .lineLimit(1)
            }
        }
        .opacity(video.watched ? 0.55 : 1)
        .contentShape(Rectangle())
    }
}

/// A video thumbnail. Unlike AsyncImage, scrolling back doesn't download and
/// decode it again (`ThumbnailCache`), and a card that scrolls away lets go of
/// its image, so a long list doesn't keep hundreds of them in memory.
struct Thumbnail: View {
    let url: URL?
    @State private var loaded: UIImage?

    var body: some View {
        Rectangle()
            .fill(.quaternary)
            .overlay {
                if let image = loaded ?? ThumbnailCache.shared.cached(url) {
                    Image(uiImage: image).resizable().aspectRatio(contentMode: .fill)
                }
            }
            .clipped()
            .task(id: url) { loaded = await ThumbnailCache.shared.image(for: url) }
            .onDisappear { loaded = nil }
    }
}

/// Decoded thumbnails, a few screens' worth (NSCache also gives them up under
/// memory pressure), over a disk cache of the JPEGs. Emptied when the app goes
/// to the background, where playback can run for hours.
final class ThumbnailCache: @unchecked Sendable {
    static let shared = ThumbnailCache()

    /// Thread-safe.
    private let images = NSCache<NSURL, UIImage>()
    private let session: URLSession

    private init() {
        // mqdefault decodes to 320×180 (230 KB): about 100 of them.
        images.totalCostLimit = 24 << 20
        let config = URLSessionConfiguration.default
        let directory = FileManager.default.urls(for: .cachesDirectory, in: .userDomainMask)[0]
            .appendingPathComponent("thumbnails", isDirectory: true)
        // ~10 KB each. Decoded images are the memory cache.
        config.urlCache = URLCache(memoryCapacity: 0, diskCapacity: 100 << 20, directory: directory)
        session = URLSession(configuration: config)
        NotificationCenter.default.addObserver(
            forName: UIApplication.didEnterBackgroundNotification, object: nil, queue: nil
        ) { [images] _ in
            images.removeAllObjects()
        }
    }

    func cached(_ url: URL?) -> UIImage? {
        url.flatMap { images.object(forKey: $0 as NSURL) }
    }

    /// Decoded off the main thread, ready to draw.
    func image(for url: URL?) async -> UIImage? {
        guard let url else { return nil }
        if let hit = cached(url) { return hit }
        guard let (data, _) = try? await session.data(from: url),
              let image = await UIImage(data: data)?.byPreparingForDisplay() else { return nil }
        let cost = image.cgImage.map { $0.bytesPerRow * $0.height } ?? data.count
        images.setObject(image, forKey: url as NSURL, cost: cost)
        return image
    }
}

/// How much of a video was played, along the bottom of its thumbnail.
struct ProgressBar: View {
    let fraction: Double

    var body: some View {
        GeometryReader { geometry in
            ZStack(alignment: .leading) {
                Rectangle().fill(.white.opacity(0.3))
                Rectangle().fill(Color.red).frame(width: geometry.size.width * min(max(fraction, 0), 1))
            }
        }
        .frame(height: 4)
        .clipShape(UnevenRoundedRectangle(bottomLeadingRadius: 12, bottomTrailingRadius: 12))
    }
}

/// Centered icon + text for empty and error states.
struct EmptyState: View {
    let icon: String
    let title: String
    let message: String

    var body: some View {
        ContentUnavailableView {
            Label(title, systemImage: icon)
        } description: {
            Text(message)
        }
        .padding(.top, 80)
    }
}
