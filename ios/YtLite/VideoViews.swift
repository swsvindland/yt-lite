import SwiftUI

/// Adaptive grid of video cards: one column on iPhone, more on iPad.
struct VideoGrid: View {
    @Environment(AppModel.self) private var model
    let videos: [Video]

    var body: some View {
        LazyVGrid(columns: [GridItem(.adaptive(minimum: 300), spacing: 16)], spacing: 20) {
            ForEach(videos) { video in
                VideoCard(video: video)
                    .onTapGesture { model.play(video) }
                    .contextMenu {
                        Button {
                            model.setWatched(video, !video.watched)
                        } label: {
                            Label(
                                video.watched ? "Mark as unwatched" : "Mark as watched",
                                systemImage: video.watched ? "eye.slash" : "eye"
                            )
                        }
                        ShareLink(item: URL(string: "https://www.youtube.com/watch?v=\(video.id)")!)
                    }
            }
        }
        .padding(.horizontal)
    }
}

struct VideoCard: View {
    let video: Video

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            ZStack(alignment: .bottomTrailing) {
                AsyncImage(url: video.thumbnailURL) { image in
                    image.resizable().aspectRatio(contentMode: .fill)
                } placeholder: {
                    Rectangle().fill(.quaternary)
                }
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
                if video.watched {
                    Rectangle()
                        .fill(Color.red)
                        .frame(height: 4)
                        .clipShape(UnevenRoundedRectangle(bottomLeadingRadius: 12, bottomTrailingRadius: 12))
                }
            }
            VStack(alignment: .leading, spacing: 3) {
                Text(video.title)
                    .font(.subheadline.weight(.semibold))
                    .lineLimit(2)
                Text("\(video.channel) · \(video.ageText)")
                    .font(.caption)
                    .foregroundStyle(.secondary)
                    .lineLimit(1)
            }
        }
        .opacity(video.watched ? 0.55 : 1)
        .contentShape(Rectangle())
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
