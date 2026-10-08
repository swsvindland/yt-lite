import AVKit
import UIKit

/// Presents the system player modally (so its ✕ button dismisses it), with
/// Picture in Picture, AirPlay, lock-screen controls and, if enabled in
/// Settings, audio that keeps playing when the phone locks or the app goes to
/// the background.
@MainActor
final class PlayerPresenter: NSObject, AVPlayerViewControllerDelegate {
    static let shared = PlayerPresenter()

    /// The controller currently showing (or in Picture in Picture).
    private var current: AVPlayerViewController?
    /// The player while detached from the controller in the background.
    private var backgroundPlayer: AVPlayer?
    private var inPictureInPicture = false

    override init() {
        super.init()
        let center = NotificationCenter.default
        center.addObserver(
            self, selector: #selector(didEnterBackground),
            name: UIApplication.didEnterBackgroundNotification, object: nil
        )
        center.addObserver(
            self, selector: #selector(willEnterForeground),
            name: UIApplication.willEnterForegroundNotification, object: nil
        )
    }

    func present(_ item: PlayItem, backgroundPlayback: Bool) {
        let playerItem = AVPlayerItem(url: item.url)
        // Cap the HLS variant; width allows up to 2:1 frames.
        playerItem.preferredMaximumResolution = CGSize(width: item.maxHeight * 2, height: item.maxHeight)

        stop()
        let player = AVPlayer(playerItem: playerItem)
        player.audiovisualBackgroundPlaybackPolicy = backgroundPlayback ? .continuesIfPossible : .pauses
        let controller = AVPlayerViewController()
        controller.player = player
        controller.allowsPictureInPicturePlayback = true
        controller.canStartPictureInPictureAutomaticallyFromInline = true
        controller.updatesNowPlayingInfoCenter = false
        controller.delegate = self
        controller.modalPresentationStyle = .fullScreen
        current = controller
        NowPlaying.shared.attach(player: player, video: item.video)

        topViewController()?.present(controller, animated: true) {
            player.play()
        }
    }

    func stop() {
        current?.player?.pause()
        backgroundPlayer?.pause()
        if current != nil { NowPlaying.shared.detach() }
        current?.dismiss(animated: false)
        current = nil
        backgroundPlayer = nil
    }

    private func topViewController() -> UIViewController? {
        let scenes = UIApplication.shared.connectedScenes.compactMap { $0 as? UIWindowScene }
        var top = scenes.flatMap(\.windows).first(where: \.isKeyWindow)?.rootViewController
        while let presented = top?.presentedViewController {
            top = presented
        }
        return top
    }

    // MARK: Background playback

    /// iOS pauses a video whose layer is on screen when the app backgrounds.
    /// Detaching the player from the view keeps the audio going (Picture in
    /// Picture handles itself).
    @objc private func didEnterBackground() {
        guard let controller = current, !inPictureInPicture,
              let player = controller.player, player.rate != 0,
              player.audiovisualBackgroundPlaybackPolicy == .continuesIfPossible else { return }
        backgroundPlayer = player
        controller.player = nil
    }

    @objc private func willEnterForeground() {
        guard let player = backgroundPlayer, let controller = current else { return }
        controller.player = player
        backgroundPlayer = nil
    }

    // MARK: AVPlayerViewControllerDelegate

    nonisolated func playerViewController(
        _ controller: AVPlayerViewController,
        willEndFullScreenPresentationWithAnimationCoordinator coordinator: UIViewControllerTransitionCoordinator
    ) {
        // ✕ tapped: stop unless Picture in Picture is taking over.
        coordinator.animate(alongsideTransition: nil) { context in
            guard !context.isCancelled else { return }
            Task { @MainActor in
                if controller.presentingViewController == nil, !self.inPictureInPicture {
                    controller.player?.pause()
                    if self.current === controller {
                        self.current = nil
                        NowPlaying.shared.detach()
                    }
                }
            }
        }
    }

    nonisolated func playerViewControllerWillStartPictureInPicture(_ controller: AVPlayerViewController) {
        Task { @MainActor in self.inPictureInPicture = true }
    }

    nonisolated func playerViewControllerDidStopPictureInPicture(_ controller: AVPlayerViewController) {
        Task { @MainActor in self.inPictureInPicture = false }
    }

    /// Picture in Picture "back to app" button: show the full player again.
    nonisolated func playerViewController(
        _ controller: AVPlayerViewController,
        restoreUserInterfaceForPictureInPictureStopWithCompletionHandler completionHandler: @escaping (Bool) -> Void
    ) {
        Task { @MainActor in
            guard controller.presentingViewController == nil, let top = self.topViewController() else {
                completionHandler(true)
                return
            }
            top.present(controller, animated: true) { completionHandler(true) }
        }
    }
}
