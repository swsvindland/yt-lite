import AVFoundation
import MediaPlayer
import UIKit

/// Lock-screen / Control Center / CarPlay Now Playing controls and metadata
/// for whichever AVPlayer is active (video or audio only).
@MainActor
final class NowPlaying {
    static let shared = NowPlaying()

    private weak var player: AVPlayer?
    private var info: [String: Any] = [:]
    private var timeObserver: Any?
    private var commandsInstalled = false

    func attach(player: AVPlayer, video: Video) {
        detach()
        self.player = player
        installCommands()
        info = [
            MPMediaItemPropertyTitle: video.title,
            MPMediaItemPropertyArtist: video.channel,
            MPNowPlayingInfoPropertyMediaType: MPNowPlayingInfoMediaType.audio.rawValue,
            MPNowPlayingInfoPropertyIsLiveStream: video.live,
        ]
        // From the listing until AVPlayer has loaded the playlist.
        if let secs = video.durationSecs, secs > 0 {
            info[MPMediaItemPropertyPlaybackDuration] = Double(secs)
        }
        publish()
        // Keep elapsed time and rate current (the lock screen extrapolates between updates).
        timeObserver = player.addPeriodicTimeObserver(
            forInterval: CMTime(seconds: 5, preferredTimescale: 1), queue: .main
        ) { [weak self] _ in
            MainActor.assumeIsolated { self?.publish() }
        }
        Task { await loadArtwork(for: video) }
    }

    func detach() {
        if let timeObserver, let player { player.removeTimeObserver(timeObserver) }
        timeObserver = nil
        player = nil
        MPNowPlayingInfoCenter.default().nowPlayingInfo = nil
    }

    func publish() {
        guard let player else { return }
        info[MPNowPlayingInfoPropertyElapsedPlaybackTime] = player.currentTime().seconds
        info[MPNowPlayingInfoPropertyPlaybackRate] = Double(player.rate)
        if let duration = player.currentItem?.duration.seconds, duration.isFinite, duration > 0 {
            info[MPMediaItemPropertyPlaybackDuration] = duration
        }
        MPNowPlayingInfoCenter.default().nowPlayingInfo = info
    }

    private func loadArtwork(for video: Video) async {
        guard let url = URL(string: "https://i.ytimg.com/vi/\(video.id)/hqdefault.jpg"),
              let (data, _) = try? await URLSession.shared.data(from: url),
              let image = UIImage(data: data) else { return }
        info[MPMediaItemPropertyArtwork] = MPMediaItemArtwork(boundsSize: image.size) { _ in image }
        publish()
    }

    private func installCommands() {
        guard !commandsInstalled else { return }
        commandsInstalled = true
        let center = MPRemoteCommandCenter.shared()
        center.playCommand.addTarget { [weak self] _ in self?.run { $0.play() } ?? .noActionableNowPlayingItem }
        center.pauseCommand.addTarget { [weak self] _ in self?.run { $0.pause() } ?? .noActionableNowPlayingItem }
        center.togglePlayPauseCommand.addTarget { [weak self] _ in
            self?.run { $0.rate == 0 ? $0.play() : $0.pause() } ?? .noActionableNowPlayingItem
        }
        center.skipForwardCommand.preferredIntervals = [15]
        center.skipBackwardCommand.preferredIntervals = [15]
        center.skipForwardCommand.addTarget { [weak self] _ in self?.run { $0.skip(by: 15) } ?? .noActionableNowPlayingItem }
        center.skipBackwardCommand.addTarget { [weak self] _ in self?.run { $0.skip(by: -15) } ?? .noActionableNowPlayingItem }
        center.changePlaybackPositionCommand.addTarget { [weak self] event in
            guard let event = event as? MPChangePlaybackPositionCommandEvent else { return .commandFailed }
            return self?.run { $0.seek(to: CMTime(seconds: event.positionTime, preferredTimescale: 600)) }
                ?? .noActionableNowPlayingItem
        }
    }

    private func run(_ action: (AVPlayer) -> Void) -> MPRemoteCommandHandlerStatus {
        guard let player else { return .noActionableNowPlayingItem }
        action(player)
        publish()
        return .success
    }
}

extension AVPlayer {
    func skip(by seconds: Double) {
        let target = max(0, currentTime().seconds + seconds)
        seek(to: CMTime(seconds: target, preferredTimescale: 600))
    }
}
