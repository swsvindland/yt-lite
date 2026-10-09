import AVFoundation
import SwiftUI

@main
struct YtLiteApp: App {
    @State private var model = AppModel.shared

    init() {
        // Keep audio playing in the background and allow Picture in Picture.
        try? AVAudioSession.sharedInstance().setCategory(.playback, mode: .moviePlayback)
    }

    var body: some Scene {
        WindowGroup {
            RootView()
                .environment(model)
                .preferredColorScheme(.dark)
                .tint(.red)
        }
    }
}

struct RootView: View {
    @Environment(AppModel.self) private var model
    @Environment(\.scenePhase) private var scenePhase

    var body: some View {
        TabView {
            FeedScreen(feed: .subscriptions, title: "Subscriptions")
                .tabItem { Label("Subs", systemImage: "rectangle.stack") }
            FeedScreen(feed: .forYou, title: "For you")
                .tabItem { Label("For you", systemImage: "star") }
            ExploreScreen()
                .tabItem { Label("Explore", systemImage: "globe") }
            SearchScreen()
                .tabItem { Label("Search", systemImage: "magnifyingglass") }
            SettingsScreen()
                .tabItem { Label("Settings", systemImage: "gearshape") }
        }
        .alert(
            "Something went wrong",
            isPresented: Binding(get: { model.errorMessage != nil }, set: { if !$0 { model.errorMessage = nil } })
        ) {
            Button("OK", role: .cancel) {}
        } message: {
            Text(model.errorMessage ?? "")
        }
        .safeAreaInset(edge: .bottom) {
            MiniPlayer()
                .padding(.bottom, 52)
        }
        .overlay {
            if model.resolving {
                ProgressView("Opening…")
                    .padding(20)
                    .background(.ultraThinMaterial, in: RoundedRectangle(cornerRadius: 14))
            }
        }
        .onChange(of: scenePhase) { _, phase in
            if phase == .active { Task { await model.checkSignIn() } }
        }
    }
}
