import Testing
@testable import SpotuifyKit

@Test func protocolVersionMatchesDaemon() {
    #expect(SpotuifyKit.ipcProtocolVersion == 4)
}
