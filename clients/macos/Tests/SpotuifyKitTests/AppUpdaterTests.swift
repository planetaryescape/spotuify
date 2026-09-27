import Testing
@testable import SpotuifyKit

struct AppUpdaterTests {
    /// The availability check and the installer must agree on where the DMG
    /// lives, or the app offers an update it then can't download.
    @Test func dmgURLPointsAtTheVersionedReleaseAsset() {
        let url = AppUpdater.dmgURL(version: "0.1.104")
        #expect(url?.absoluteString
            == "https://github.com/planetaryescape/spotuify/releases/download/v0.1.104/Spotuify-0.1.104.dmg")
    }
}
