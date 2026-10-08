import AuthenticationServices
import UIKit

/// Shows Google's consent page in an in-app browser sheet. Google redirects
/// to a loopback port the Rust core listens on (the same "Desktop app" OAuth
/// client the desktop app uses), so this session never sees a callback URL;
/// the caller closes it once the core has the code.
enum WebAuth {
    @MainActor
    static func start(url: URL, onUserCancel: @escaping @Sendable () -> Void) -> ASWebAuthenticationSession {
        let session = ASWebAuthenticationSession(url: url, callbackURLScheme: "ytlite-unused") { _, _ in
            // Fires when the user closes the sheet (or when we cancel it).
            onUserCancel()
        }
        session.presentationContextProvider = Presenter.shared
        session.prefersEphemeralWebBrowserSession = false
        session.start()
        return session
    }

    private final class Presenter: NSObject, ASWebAuthenticationPresentationContextProviding {
        static let shared = Presenter()

        func presentationAnchor(for session: ASWebAuthenticationSession) -> ASPresentationAnchor {
            let scenes = UIApplication.shared.connectedScenes.compactMap { $0 as? UIWindowScene }
            return scenes.flatMap(\.windows).first(where: \.isKeyWindow) ?? ASPresentationAnchor()
        }
    }
}
