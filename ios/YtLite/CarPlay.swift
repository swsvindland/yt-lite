import CarPlay
import UIKit

/// The CarPlay scene, declared in Info.plist. CarPlay only shows apps with
/// the `com.apple.developer.carplay-audio` entitlement (see Config.xcconfig).
@MainActor
final class CarPlaySceneDelegate: UIResponder, CPTemplateApplicationSceneDelegate {
    private var controller: CarPlayController?

    func templateApplicationScene(
        _ templateApplicationScene: CPTemplateApplicationScene,
        didConnect interfaceController: CPInterfaceController
    ) {
        controller = CarPlayController(interface: interfaceController)
    }

    func templateApplicationScene(
        _ templateApplicationScene: CPTemplateApplicationScene,
        didDisconnectInterfaceController interfaceController: CPInterfaceController
    ) {
        controller = nil
    }
}

/// Tabs for Subscriptions, For you and Explore. Choosing a video plays it
/// audio only (never video in the car) and opens the system Now Playing
/// screen, whose metadata and controls come from `NowPlaying`.
@MainActor
final class CarPlayController: NSObject, CPTabBarTemplateDelegate {
    private let interface: CPInterfaceController
    private let model = AppModel.shared
    private let feeds: [VideoList]
    /// The Explore topic on screen, if any.
    private var topic: VideoList?
    private let thumbnails = NSCache<NSString, UIImage>()

    init(interface: CPInterfaceController) {
        self.interface = interface
        let model = model
        feeds = [
            VideoList(
                title: "Subscriptions", icon: "rectangle.stack", feed: .subscriptions,
                empty: ("No videos yet", "New uploads from your subscriptions show up here."),
                load: { try await model.videos(.subscriptions) },
                fetch: { try await model.refresh(.subscriptions) }
            ),
            VideoList(
                title: "For you", icon: "star", feed: .forYou,
                empty: ("Nothing here yet", "Watch a few videos and yt-lite will recommend more like them."),
                load: { try await model.videos(.forYou) },
                fetch: { try await model.refresh(.forYou) }
            ),
        ]
        super.init()
        let tabs = CPTabBarTemplate(templates: feeds.map(\.template) + [exploreTemplate()])
        tabs.delegate = self
        interface.setRootTemplate(tabs, animated: false, completion: nil)
        for list in feeds {
            Task { await update(list) }
        }
        observe()
    }

    func tabBarTemplate(_ tabBarTemplate: CPTabBarTemplate, didSelect selectedTemplate: CPTemplate) {
        guard let list = feeds.first(where: { $0.template === selectedTemplate }) else { return }
        Task { await update(list) }
    }

    // MARK: Lists

    private func exploreTemplate() -> CPListTemplate {
        let items = model.exploreTopics.enumerated().map { index, name in
            let item = CPListItem(text: name, detailText: nil)
            item.accessoryType = .disclosureIndicator
            item.handler = { [weak self] _, completion in
                self?.open(topic: index, name: name)
                completion()
            }
            return item
        }
        let template = CPListTemplate(title: "Explore", sections: [CPListSection(items: items)])
        template.tabTitle = "Explore"
        template.tabImage = UIImage(systemName: "globe")
        return template
    }

    private func open(topic index: Int, name: String) {
        let model = model
        let list = VideoList(
            title: name, empty: ("No videos", "Nothing popular here this week."),
            load: { try await model.explore(topic: index) }
        )
        topic = list
        render(list)
        interface.pushTemplate(list.template, animated: true, completion: nil)
        Task { await update(list) }
    }

    /// Shows the cached videos, then refreshes from YouTube if they're more
    /// than 15 minutes old.
    private func update(_ list: VideoList) async {
        guard let fetch = list.fetch, !list.fetching,
              list.fetchedAt.map({ Date.now.timeIntervalSince($0) > 15 * 60 }) ?? true else {
            await reload(list)
            return
        }
        list.fetching = true
        await reload(list)
        do {
            try await fetch()
            list.fetchedAt = .now
            list.failure = nil
        } catch FfiError.NotSignedIn {
            list.failure = nil
        } catch {
            list.failure = describe(error)
        }
        list.fetching = false
        await reload(list)
    }

    private func reload(_ list: VideoList) async {
        do {
            list.videos = try await list.load()
        } catch FfiError.NotSignedIn {
        } catch {
            list.failure = describe(error)
        }
        list.loaded = true
        render(list)
    }

    private func render(_ list: VideoList) {
        let items = list.videos.prefix(Int(CPListTemplate.maximumItemCount)).map(item(for:))
        list.template.updateSections([CPListSection(items: items)])
        let loading = !list.loaded || list.fetching
        let (title, subtitle): (String, String) =
            if loading { ("Loading…", "") }
            else if let failure = list.failure { ("Couldn't load", failure) }
            else if list.feed == .subscriptions && !model.signedIn {
                ("Sign in on your iPhone", "Open yt-lite and connect your YouTube account in Settings.")
            } else { list.empty }
        list.template.emptyViewTitleVariants = [title]
        list.template.emptyViewSubtitleVariants = subtitle.isEmpty ? [] : [subtitle]
        if #available(iOS 18.4, *) {
            list.template.showsSpinnerWhileEmpty = loading
        }
    }

    private func item(for video: Video) -> CPListItem {
        let cached = thumbnails.object(forKey: video.id as NSString)
        let item = CPListItem(
            text: video.title,
            detailText: [video.channel, video.durationText, video.ageText].compactMap { $0 }.joined(separator: " · "),
            image: cached ?? UIImage(systemName: "play.rectangle")
        )
        item.isPlaying = AudioPlayer.shared.current?.id == video.id
        item.playbackProgress = video.watched ? 1 : 0
        item.handler = { [weak self] _, completion in
            Task {
                await self?.listen(video)
                completion()
            }
        }
        if cached == nil {
            Task { [weak self] in
                if let image = await self?.thumbnail(for: video) { item.setImage(image) }
            }
        }
        return item
    }

    // MARK: Playback

    private func listen(_ video: Video) async {
        do {
            try await model.start(video, audioOnly: true)
        } catch {
            let ok = CPAlertAction(title: "OK", style: .cancel) { [weak self] _ in
                self?.interface.dismissTemplate(animated: true, completion: nil)
            }
            let alert = CPAlertTemplate(titleVariants: [describe(error), "Couldn't play this video"], actions: [ok])
            interface.presentTemplate(alert, animated: true, completion: nil)
            return
        }
        guard AudioPlayer.shared.current?.id == video.id,
              interface.topTemplate !== CPNowPlayingTemplate.shared else { return }
        interface.pushTemplate(CPNowPlayingTemplate.shared, animated: true, completion: nil)
    }

    /// Re-renders when playback, watched state or sign-in change (on the
    /// phone or here).
    private func observe() {
        withObservationTracking {
            _ = AudioPlayer.shared.current?.id
            _ = model.watchedVersion
            _ = model.signedIn
        } onChange: { [weak self] in
            Task { @MainActor in
                guard let self else { return }
                self.observe()
                if let topic = self.topic { self.render(topic) }
                for list in self.feeds {
                    Task { await self.update(list) }
                }
            }
        }
    }

    // MARK: Thumbnails

    private func thumbnail(for video: Video) async -> UIImage? {
        guard let url = video.thumbnailURL,
              let (data, _) = try? await URLSession.shared.data(from: url),
              let image = UIImage(data: data) else { return nil }
        let art = square(image)
        thumbnails.setObject(art, forKey: video.id as NSString)
        return art
    }

    /// Center crop to the list's square image slot, at the car screen's scale.
    private func square(_ image: UIImage) -> UIImage {
        let side = CPListItem.maximumImageSize
        let format = UIGraphicsImageRendererFormat()
        format.scale = interface.carTraitCollection.displayScale
        return UIGraphicsImageRenderer(size: side, format: format).image { _ in
            let scale = max(side.width / image.size.width, side.height / image.size.height)
            let size = CGSize(width: image.size.width * scale, height: image.size.height * scale)
            image.draw(in: CGRect(
                x: (side.width - size.width) / 2, y: (side.height - size.height) / 2,
                width: size.width, height: size.height
            ))
        }
    }
}

/// A list of videos in CarPlay: a feed tab or an Explore topic.
@MainActor
private final class VideoList {
    let template: CPListTemplate
    let feed: Feed?
    /// Title and subtitle when there are no videos.
    let empty: (String, String)
    /// What to show (the cache for feeds; a network call for Explore).
    let load: () async throws -> [Video]
    /// Refreshes the cache from YouTube; nil for Explore topics.
    let fetch: (() async throws -> Void)?

    var videos: [Video] = []
    var loaded = false
    var fetching = false
    var fetchedAt: Date?
    var failure: String?

    init(
        title: String, icon: String? = nil, feed: Feed? = nil, empty: (String, String),
        load: @escaping () async throws -> [Video], fetch: (() async throws -> Void)? = nil
    ) {
        template = CPListTemplate(title: title, sections: [])
        template.tabTitle = title
        if let icon { template.tabImage = UIImage(systemName: icon) }
        self.feed = feed
        self.empty = empty
        self.load = load
        self.fetch = fetch
    }
}
