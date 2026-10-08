import AVKit
import UIKit

/// Presents the system player modally (so its ✕ button dismisses it), with
/// Picture in Picture, AirPlay and background audio.
@MainActor
final class PlayerPresenter: NSObject, AVPlayerViewControllerDelegate {
    static let shared = PlayerPresenter()

    /// The controller currently showing (or in Picture in Picture).
    private var current: AVPlayerViewController?

    func present(_ item: PlayItem) {
        let playerItem = AVPlayerItem(url: item.url)
        // Cap the HLS variant; width allows up to 2:1 frames.
        playerItem.preferredMaximumResolution = CGSize(width: item.maxHeight * 2, height: item.maxHeight)

        stop()
        let controller = AVPlayerViewController()
        controller.player = AVPlayer(playerItem: playerItem)
        controller.allowsPictureInPicturePlayback = true
        controller.canStartPictureInPictureAutomaticallyFromInline = true
        controller.delegate = self
        controller.modalPresentationStyle = .fullScreen
        current = controller

        topViewController()?.present(controller, animated: true) {
            controller.player?.play()
        }
    }

    private func stop() {
        current?.player?.pause()
        current?.dismiss(animated: false)
        current = nil
    }

    private func topViewController() -> UIViewController? {
        let scenes = UIApplication.shared.connectedScenes.compactMap { $0 as? UIWindowScene }
        var top = scenes.flatMap(\.windows).first(where: \.isKeyWindow)?.rootViewController
        while let presented = top?.presentedViewController {
            top = presented
        }
        return top
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
                    if self.current === controller { self.current = nil }
                }
            }
        }
    }

    private var inPictureInPicture = false

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
