import SwiftUI

/// Subscriptions or For you: cached list first, then a refresh.
struct FeedScreen: View {
    @Environment(AppModel.self) private var model
    let feed: Feed
    let title: String

    @State private var videos: [Video] = []
    @State private var loading = false
    @State private var failure: String?
    @State private var lastRefresh: Date?

    var body: some View {
        NavigationStack {
            ScrollView {
                if videos.isEmpty {
                    emptyState
                } else {
                    VideoGrid(videos: videos).padding(.top, 8)
                }
            }
            .navigationTitle(title)
            .refreshable { await refresh() }
            .overlay(alignment: .top) {
                if loading && videos.isEmpty { ProgressView().padding(.top, 120) }
            }
        }
        .task {
            await reload()
            if lastRefresh.map({ Date.now.timeIntervalSince($0) > 15 * 60 }) ?? true {
                await refresh()
            }
        }
        .onChange(of: model.watchedVersion) { Task { await reload() } }
        .onChange(of: model.signedIn) { _, signedIn in
            if signedIn && feed == .subscriptions { Task { await refresh() } }
        }
    }

    @ViewBuilder private var emptyState: some View {
        if let failure {
            EmptyState(icon: "exclamationmark.triangle", title: "Couldn't load", message: failure)
        } else if loading {
            EmptyView()
        } else if feed == .subscriptions && !model.signedIn {
            VStack(spacing: 16) {
                EmptyState(
                    icon: "person.crop.circle",
                    title: "Connect your YouTube account",
                    message: "Sign in to load the channels you subscribe to."
                )
                if model.hasGoogleClient {
                    Button {
                        model.signIn()
                    } label: {
                        Label("Sign in with Google", systemImage: "person.crop.circle")
                    }
                    .buttonStyle(.borderedProminent)
                    .disabled(model.signingIn)
                }
            }
        } else if feed == .forYou {
            EmptyState(icon: "star", title: "Nothing here yet", message: "Watch a few videos and yt-lite will recommend more like them.")
        } else {
            EmptyState(icon: "tray", title: "No videos yet", message: "Pull down to refresh.")
        }
    }

    private func reload() async {
        if let fresh = try? await model.videos(feed) { videos = fresh }
    }

    private func refresh() async {
        loading = true
        defer { loading = false }
        do {
            try await model.refresh(feed)
            failure = nil
            lastRefresh = .now
        } catch FfiError.NotSignedIn {
            failure = nil
        } catch {
            failure = describe(error)
        }
        await reload()
    }
}

/// Popular this week, by topic.
struct ExploreScreen: View {
    @Environment(AppModel.self) private var model
    @State private var topic = 0
    @State private var videos: [Video] = []
    @State private var loading = false
    @State private var failure: String?

    var body: some View {
        NavigationStack {
            ScrollView {
                ScrollView(.horizontal, showsIndicators: false) {
                    HStack(spacing: 8) {
                        ForEach(Array(model.exploreTopics.enumerated()), id: \.offset) { index, name in
                            Button(name) {
                                topic = index
                                Task { await load() }
                            }
                            .buttonStyle(.bordered)
                            .tint(index == topic ? .red : .secondary)
                        }
                    }
                    .padding(.horizontal)
                }
                .padding(.vertical, 8)
                if loading && videos.isEmpty {
                    ProgressView().padding(.top, 80)
                } else if let failure, videos.isEmpty {
                    EmptyState(icon: "exclamationmark.triangle", title: "Couldn't load", message: failure)
                } else {
                    VideoGrid(videos: videos)
                }
            }
            .navigationTitle("Explore")
            .refreshable { await load() }
        }
        .task { if videos.isEmpty { await load() } }
    }

    private func load() async {
        loading = true
        defer { loading = false }
        do {
            videos = try await model.explore(topic: topic)
            failure = nil
        } catch {
            failure = describe(error)
        }
    }
}

struct SearchScreen: View {
    @Environment(AppModel.self) private var model
    @State private var query = ""
    @State private var videos: [Video] = []
    @State private var loading = false
    @State private var failure: String?
    @State private var searched = false

    var body: some View {
        NavigationStack {
            ScrollView {
                if loading {
                    ProgressView().padding(.top, 80)
                } else if let failure {
                    EmptyState(icon: "exclamationmark.triangle", title: "Search failed", message: failure)
                } else if videos.isEmpty {
                    EmptyState(
                        icon: "magnifyingglass",
                        title: searched ? "No results" : "Search YouTube",
                        message: "Results never include Shorts."
                    )
                } else {
                    VideoGrid(videos: videos).padding(.top, 8)
                }
            }
            .navigationTitle("Search")
            .searchable(text: $query, placement: .navigationBarDrawer(displayMode: .always), prompt: "Search YouTube")
            .onSubmit(of: .search) { Task { await run() } }
        }
    }

    private func run() async {
        let q = query.trimmingCharacters(in: .whitespaces)
        guard !q.isEmpty else { return }
        loading = true
        defer { loading = false }
        do {
            videos = try await model.search(q)
            failure = nil
        } catch {
            failure = describe(error)
        }
        searched = true
    }
}

struct SettingsScreen: View {
    @Environment(AppModel.self) private var model

    var body: some View {
        @Bindable var model = model
        NavigationStack {
            Form {
                Section("YouTube account") {
                    if model.signedIn {
                        Label("Signed in", systemImage: "checkmark.circle.fill").foregroundStyle(.green)
                        Button("Sign out", role: .destructive) { model.signOut() }
                    } else if model.hasGoogleClient {
                        Button {
                            model.signIn()
                        } label: {
                            Label(model.signingIn ? "Waiting for Google…" : "Sign in with Google", systemImage: "person.crop.circle")
                        }
                        .disabled(model.signingIn)
                    } else {
                        Text("No Google OAuth client was bundled. Run scripts/build-ios.sh on a Mac whose desktop config has one.")
                            .font(.footnote)
                            .foregroundStyle(.secondary)
                    }
                }
                Section("Playback") {
                    Picker("Maximum quality", selection: $model.maxHeight) {
                        Text("720p").tag(720)
                        Text("1080p").tag(1080)
                        Text("1440p").tag(1440)
                        Text("4K").tag(2160)
                    }
                }
                Section("Feeds") {
                    Toggle("Hide watched videos", isOn: $model.hideWatched)
                }
                Section {
                    LabeledContent("Shorts", value: "Never shown")
                    LabeledContent("Ads", value: "None")
                } footer: {
                    Text("yt-lite resolves streams natively and plays them with the system player.")
                }
            }
            .navigationTitle("Settings")
        }
    }
}
