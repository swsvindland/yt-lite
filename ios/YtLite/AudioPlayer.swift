import AVFoundation
import Observation
import SwiftUI

/// Audio-only playback (podcast mode): the HLS audio-only rendition, no
/// video decoding, keeps playing with the screen off. Shown as a mini player
/// on the phone and as Now Playing in CarPlay.
@Observable
@MainActor
final class AudioPlayer {
    static let shared = AudioPlayer()

    private(set) var current: Video?
    private(set) var isPlaying = false
    private(set) var elapsed: Double = 0
    private(set) var duration: Double = 0

    @ObservationIgnored private var player: AVPlayer?
    @ObservationIgnored private var timeObserver: Any?
    @ObservationIgnored private var rateObserver: NSKeyValueObservation?

    func play(url: URL, video: Video) {
        stop()
        let item = AVPlayerItem(url: url)
        // Only matters if the core had to return the master playlist (no
        // audio-only rendition): take its smallest variant.
        item.preferredPeakBitRate = 1
        let player = AVPlayer(playerItem: item)
        player.audiovisualBackgroundPlaybackPolicy = .continuesIfPossible
        self.player = player
        current = video
        duration = Double(video.durationSecs ?? 0)
        timeObserver = player.addPeriodicTimeObserver(
            forInterval: CMTime(seconds: 0.5, preferredTimescale: 600), queue: .main
        ) { [weak self] time in
            MainActor.assumeIsolated {
                guard let self else { return }
                self.elapsed = time.seconds
                if let d = self.player?.currentItem?.duration.seconds, d.isFinite, d > 0 {
                    self.duration = d
                }
            }
        }
        rateObserver = player.observe(\.rate, options: [.new]) { [weak self] player, _ in
            let playing = player.rate != 0
            Task { @MainActor in
                self?.isPlaying = playing
                NowPlaying.shared.publish()
            }
        }
        try? AVAudioSession.sharedInstance().setActive(true)
        NowPlaying.shared.attach(player: player, video: video)
        player.play()
    }

    func togglePlay() {
        guard let player else { return }
        player.rate == 0 ? player.play() : player.pause()
    }

    func skip(_ seconds: Double) {
        player?.skip(by: seconds)
        NowPlaying.shared.publish()
    }

    func seek(to fraction: Double) {
        guard duration > 0 else { return }
        player?.seek(to: CMTime(seconds: duration * fraction, preferredTimescale: 600))
    }

    func stop() {
        if let timeObserver, let player { player.removeTimeObserver(timeObserver) }
        timeObserver = nil
        rateObserver = nil
        player?.pause()
        if player != nil { NowPlaying.shared.detach() }
        player = nil
        current = nil
        isPlaying = false
        elapsed = 0
    }
}

/// Mini player above the tab bar while audio-only playback is active.
struct MiniPlayer: View {
    @State private var audio = AudioPlayer.shared

    var body: some View {
        if let video = audio.current {
            VStack(spacing: 0) {
                HStack(spacing: 12) {
                    AsyncImage(url: video.thumbnailURL) { image in
                        image.resizable().aspectRatio(contentMode: .fill)
                    } placeholder: {
                        Rectangle().fill(.quaternary)
                    }
                    .frame(width: 64, height: 36)
                    .clipShape(RoundedRectangle(cornerRadius: 6))

                    VStack(alignment: .leading, spacing: 2) {
                        Text(video.title).font(.footnote.weight(.semibold)).lineLimit(1)
                        Text(video.live ? "LIVE · \(video.channel)" : "\(clock(audio.elapsed)) / \(clock(audio.duration)) · \(video.channel)")
                            .font(.caption2)
                            .foregroundStyle(.secondary)
                            .lineLimit(1)
                            .monospacedDigit()
                    }
                    Spacer(minLength: 4)
                    Button { audio.skip(-15) } label: { Image(systemName: "gobackward.15") }
                    Button { audio.togglePlay() } label: {
                        Image(systemName: audio.isPlaying ? "pause.fill" : "play.fill").font(.title3)
                    }
                    .frame(width: 32)
                    Button { audio.skip(30) } label: { Image(systemName: "goforward.30") }
                    Button { audio.stop() } label: { Image(systemName: "xmark").font(.footnote) }
                        .foregroundStyle(.secondary)
                }
                .buttonStyle(.plain)
                .padding(.horizontal, 14)
                .padding(.vertical, 10)

                ProgressView(value: !video.live && audio.duration > 0 ? min(audio.elapsed / audio.duration, 1) : 0)
                    .progressViewStyle(.linear)
                    .tint(.red)
            }
            .background(.regularMaterial, in: RoundedRectangle(cornerRadius: 16))
            .padding(.horizontal, 12)
            .padding(.bottom, 4)
        }
    }

    private func clock(_ secs: Double) -> String {
        guard secs.isFinite, secs > 0 else { return "0:00" }
        let s = Int(secs)
        return s >= 3600
            ? String(format: "%d:%02d:%02d", s / 3600, (s % 3600) / 60, s % 60)
            : String(format: "%d:%02d", s / 60, s % 60)
    }
}
