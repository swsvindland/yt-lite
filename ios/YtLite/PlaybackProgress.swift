import AVFoundation
import UIKit

/// Saves where playback got to, so the video resumes there next time; the
/// core marks it watched once it has played to the end. Follows whichever
/// AVPlayer is active (video or audio only), like `NowPlaying`.
@MainActor
final class PlaybackProgress {
    static let shared = PlaybackProgress()

    private weak var player: AVPlayer?
    private var video: Video?
    private var timeObserver: Any?
    private var rateObserver: NSKeyValueObservation?
    private var observers: [NSObjectProtocol] = []

    func attach(player: AVPlayer, video: Video) {
        detach()
        // Live streams have no position to come back to.
        guard !video.live else { return }
        self.player = player
        self.video = video
        // Often enough that little is lost if iOS ends the app in the background.
        timeObserver = player.addPeriodicTimeObserver(
            forInterval: CMTime(seconds: 10, preferredTimescale: 1), queue: .main
        ) { [weak self] _ in
            MainActor.assumeIsolated { self?.save(refresh: false) }
        }
        // Paused: by the user, a call, unplugged headphones, or the end.
        rateObserver = player.observe(\.rate, options: [.new]) { [weak self] player, _ in
            guard player.rate == 0 else { return }
            Task { @MainActor in self?.save(refresh: true) }
        }
        let center = NotificationCenter.default
        observers = [
            center.addObserver(
                forName: AVPlayerItem.didPlayToEndTimeNotification, object: player.currentItem, queue: .main
            ) { [weak self] _ in
                MainActor.assumeIsolated { self?.save(refresh: true) }
            },
            center.addObserver(
                forName: UIApplication.didEnterBackgroundNotification, object: nil, queue: .main
            ) { [weak self] _ in
                MainActor.assumeIsolated { self?.save(refresh: false) }
            },
        ]
    }

    /// Saves the final position and stops following the player. With a
    /// `player`, only if that's the one being followed.
    func detach(_ player: AVPlayer? = nil) {
        if let player, player !== self.player { return }
        save(refresh: true)
        if let timeObserver, let attached = self.player { attached.removeTimeObserver(timeObserver) }
        observers.forEach(NotificationCenter.default.removeObserver)
        timeObserver = nil
        rateObserver = nil
        observers = []
        self.player = nil
        video = nil
    }

    /// `refresh`: reload the lists so their progress bars catch up.
    private func save(refresh: Bool) {
        // Before the item is ready its time is 0, which would erase the resume point.
        guard let player, let video, let item = player.currentItem, item.status == .readyToPlay else { return }
        let duration = item.duration.seconds
        AppModel.shared.saveProgress(
            video,
            position: player.currentTime().seconds,
            duration: duration.isFinite && duration > 0 ? duration : nil,
            refresh: refresh
        )
    }
}
