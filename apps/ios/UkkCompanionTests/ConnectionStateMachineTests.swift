import XCTest
@testable import UkkCompanion

final class ConnectionStateMachineTests: XCTestCase {
    func testFixtureStartIsNeverLive() {
        let machine = ConnectionStateMachine()
        XCTAssertEqual(machine.state, .fixtureData)
        XCTAssertFalse(machine.isLiveClaim)
        XCTAssertFalse(machine.canDispatchCommands)
        XCTAssertFalse(machine.indicator.isLive)
        XCTAssertFalse(machine.isRetryAvailable)
        XCTAssertTrue(machine.indicator.label.contains("Fixture data"))
    }

    func testDialConnectingThenOpenBecomesLive() {
        var machine = ConnectionStateMachine(initial: .fixtureData)
        machine.handle(.dialStarted)
        XCTAssertEqual(machine.state, .connecting)
        XCTAssertFalse(machine.isLiveClaim)

        machine.handle(.transportOpened)
        XCTAssertEqual(machine.state, .online)
        XCTAssertTrue(machine.isLiveClaim)
        XCTAssertEqual(machine.indicator.label, "Live")
    }

    func testTransportCloseStartsReconnectingAndNeverClaimsLive() {
        var machine = ConnectionStateMachine(initial: .online)
        machine.handle(.transportClosed)
        XCTAssertEqual(machine.state, .reconnecting(attempt: 1))
        XCTAssertFalse(machine.isLiveClaim)
        XCTAssertFalse(machine.canDispatchCommands)
        XCTAssertTrue(machine.indicator.label.contains("Reconnecting"))
    }

    func testDegradedIsNotLiveAndFreshRecovers() {
        var machine = ConnectionStateMachine(initial: .online)
        machine.handle(.connectionDegraded("relay closed"))
        XCTAssertEqual(machine.state, .degraded("relay closed"))
        XCTAssertFalse(machine.isLiveClaim)
        XCTAssertFalse(machine.indicator.isLive)
        XCTAssertEqual(machine.indicator.label, "Degraded")

        machine.handle(.dataFresh)
        XCTAssertEqual(machine.state, .online)
    }

    func testReconnectFailuresBackOffThenGoOffline() {
        var machine = ConnectionStateMachine(initial: .reconnecting(attempt: 1), maxReconnectAttempts: 3)

        machine.handle(.reconnectAttemptFailed)
        XCTAssertEqual(machine.state, .reconnecting(attempt: 2))
        machine.handle(.reconnectAttemptFailed)
        XCTAssertEqual(machine.state, .reconnecting(attempt: 3))
        machine.handle(.reconnectAttemptFailed)
        XCTAssertEqual(machine.state, .offline(.hostUnreachable))
        XCTAssertTrue(machine.isRetryAvailable)
        XCTAssertFalse(machine.isLiveClaim)
    }

    func testHostClientReconnectScheduleAndOfflineMapping() {
        var machine = ConnectionStateMachine(initial: .degraded("relay closed"))
        machine.handle(.reconnectScheduled(3))
        XCTAssertEqual(machine.state, .reconnecting(attempt: 3))
        machine.handle(.wentOffline(.hostUnreachable))
        XCTAssertEqual(machine.state, .offline(.hostUnreachable))
    }

    func testUserRetryFromOfflineStartsConnecting() {
        var machine = ConnectionStateMachine(initial: .offline(.transportClosed))
        machine.handle(.userRequestedRetry)
        XCTAssertEqual(machine.state, .connecting)
    }

    func testUserRetryFromFixtureStartsConnectingButIsStillNotLive() {
        var machine = ConnectionStateMachine()
        machine.handle(.userRequestedRetry)
        XCTAssertEqual(machine.state, .connecting)
        XCTAssertFalse(machine.isLiveClaim)
    }

    func testBackgroundSuspendsAndForegroundRequiresFreshHandshake() {
        var machine = ConnectionStateMachine(initial: .online)
        machine.handle(.appEnteredBackground)
        XCTAssertEqual(machine.state, .suspended)
        XCTAssertFalse(machine.isLiveClaim)

        machine.handle(.appEnteredForeground)
        XCTAssertEqual(machine.state, .connecting)
        XCTAssertFalse(machine.isLiveClaim)

        machine.handle(.transportOpened)
        XCTAssertEqual(machine.state, .online)
    }

    func testFixtureModeSurvivesBackgroundAndForeground() {
        var machine = ConnectionStateMachine()
        machine.handle(.appEnteredBackground)
        XCTAssertEqual(machine.state, .fixtureData)
        machine.handle(.appEnteredForeground)
        XCTAssertEqual(machine.state, .fixtureData)
        XCTAssertFalse(machine.isLiveClaim)
        XCTAssertFalse(machine.isRetryAvailable)
    }

    func testOfflineStateSurvivesBackground() {
        var machine = ConnectionStateMachine(initial: .offline(.hostUnreachable))
        machine.handle(.appEnteredBackground)
        XCTAssertEqual(machine.state, .offline(.hostUnreachable))
        machine.handle(.appEnteredForeground)
        XCTAssertEqual(machine.state, .offline(.hostUnreachable))
    }

    func testTransportOpenedWhileSuspendedIsIgnored() {
        var machine = ConnectionStateMachine(initial: .online)
        machine.handle(.appEnteredBackground)
        machine.handle(.transportOpened)
        XCTAssertEqual(machine.state, .suspended)
        machine.handle(.appEnteredForeground)
        machine.handle(.transportOpened)
        XCTAssertEqual(machine.state, .online)
    }

    func testPairingRequiredIsOfflineWithActionableCopy() {
        var machine = ConnectionStateMachine(initial: .connecting)
        machine.handle(.pairingRequired)
        XCTAssertEqual(machine.state, .offline(.pairingRequired))
        XCTAssertTrue(machine.indicator.label.contains("Pairing"))
        XCTAssertTrue(machine.isRetryAvailable)
    }

    func testRevocationIsTerminal() {
        var machine = ConnectionStateMachine(initial: .online)
        machine.handle(.deviceRevoked)
        XCTAssertEqual(machine.state, .revoked)
        machine.handle(.transportOpened)
        machine.handle(.appEnteredForeground)
        machine.handle(.userRequestedRetry)
        XCTAssertEqual(machine.state, .revoked)
        XCTAssertFalse(machine.isLiveClaim)
        XCTAssertEqual(machine.indicator.label, "Device revoked")
    }

    func testForgottenHostReturnsToFixtureMode() {
        var machine = ConnectionStateMachine(initial: .online)
        machine.handle(.hostForgotten)
        XCTAssertEqual(machine.state, .fixtureData)
        XCTAssertFalse(machine.isConfigured)
    }

    func testIndicatorNeverClaimsLiveOutsideOnline() {
        let states: [ConnectionState] = [
            .fixtureData,
            .connecting,
            .degraded("stale"),
            .reconnecting(attempt: 2),
            .offline(.hostUnreachable),
            .offline(.transportClosed),
            .offline(.pairingRequired),
            .suspended,
            .revoked
        ]
        for state in states {
            let machine = ConnectionStateMachine(initial: state)
            XCTAssertFalse(machine.indicator.isLive, "state \(state) must not claim live")
            XCTAssertFalse(machine.isLiveClaim, "state \(state) must not allow dispatch")
        }
    }

    func testRetryDelayIsBoundedAndDeterministic() {
        XCTAssertEqual(ConnectionStateMachine.retryDelay(forAttempt: 0), 1)
        XCTAssertEqual(ConnectionStateMachine.retryDelay(forAttempt: 1), 1)
        XCTAssertEqual(ConnectionStateMachine.retryDelay(forAttempt: 2), 2)
        XCTAssertEqual(ConnectionStateMachine.retryDelay(forAttempt: 3), 4)
        XCTAssertEqual(ConnectionStateMachine.retryDelay(forAttempt: 4), 8)
        XCTAssertEqual(ConnectionStateMachine.retryDelay(forAttempt: 5), 16)
        XCTAssertEqual(ConnectionStateMachine.retryDelay(forAttempt: 6), 30)
        XCTAssertEqual(ConnectionStateMachine.retryDelay(forAttempt: 99), 30)
    }

    func testReconnectAttemptIsClampedToOne() {
        let machine = ConnectionStateMachine(maxReconnectAttempts: 0)
        XCTAssertEqual(machine.maxReconnectAttempts, 1)
    }
}
