import XCTest
@testable import UkkCompanion

final class RemoteAPITests: XCTestCase {
    func testCorrelatesResponseByIdAndCarriesParams() async throws {
        let sent = Box<[Data]>([])
        let events = Box<[WireEvent]>([])
        let api = RemoteAPI(
            sender: { data in sent.modify { $0.append(data) } },
            onEvent: { event in events.modify { $0.append(event) } }
        )

        let task = Task { try await api.request(.agentSnapshot, params: WireParams.session("session-1")) }
        guard await waitUntil({ sent.value.count == 1 }) else { return XCTFail("request not sent") }

        let request = try XCTUnwrap(sent.value.first)
        let object = try XCTUnwrap(try JSONSerialization.jsonObject(with: request) as? [String: Any])
        XCTAssertEqual(object["method"] as? String, "agent.snapshot")
        let id = try XCTUnwrap(object["id"] as? String)
        XCTAssertEqual(id.count, 16)
        XCTAssertEqual((object["params"] as? [String: Any])?["session_id"] as? String, "session-1")

        await api.ingest(json(["id": id, "result": ["ok": true]]))
        let result = try await task.value
        XCTAssertEqual(result, .object(["ok": .bool(true)]))
    }

    func testServerErrorsAreTyped() async throws {
        let sent = Box<[Data]>([])
        let api = RemoteAPI(
            sender: { data in sent.modify { $0.append(data) } },
            onEvent: { _ in }
        )

        let task = Task { try await api.request(.workspaceGet) }
        guard await waitUntil({ sent.value.count == 1 }),
              let request = sent.value.first,
              let object = try JSONSerialization.jsonObject(with: request) as? [String: Any],
              let id = object["id"] as? String else {
            return XCTFail("request not sent")
        }
        await api.ingest(json(["id": id, "error": ["code": "device_revoked", "message": "revoked"]]))

        do {
            _ = try await task.value
            XCTFail("expected revocation")
        } catch {
            XCTAssertEqual(error as? RemoteAPIError, .revoked)
        }
    }

    func testForbiddenIsTyped() async throws {
        let sent = Box<[Data]>([])
        let api = RemoteAPI(
            sender: { data in sent.modify { $0.append(data) } },
            onEvent: { _ in }
        )
        let task = Task { try await api.request(.agentPrompt, params: WireParams.prompt(sessionID: "s", text: "hi")) }
        guard await waitUntil({ sent.value.count == 1 }),
              let request = sent.value.first,
              let object = try JSONSerialization.jsonObject(with: request) as? [String: Any],
              let id = object["id"] as? String else {
            return XCTFail("request not sent")
        }
        await api.ingest(json(["id": id, "error": ["code": "missing_scope", "message": "agent.send required"]]))

        do {
            _ = try await task.value
            XCTFail("expected forbidden")
        } catch {
            XCTAssertEqual(error as? RemoteAPIError, .forbidden("agent.send required"))
        }
    }

    func testEventsAreDelivered() async throws {
        let events = Box<[WireEvent]>([])
        let api = RemoteAPI(sender: { _ in }, onEvent: { event in events.modify { $0.append(event) } })

        let payload: [String: Any] = [
            "type": "event",
            "event": [
                "type": "update",
                "session_id": "session-1",
                "at_ms": 1_762_550_000_000,
                "agent": [
                    "provider": "codex",
                    "state": "running",
                    "messages": [],
                    "turns": [],
                    "tools": [],
                    "pending_approvals": []
                ]
            ]
        ]
        await api.ingest(json(payload))

        XCTAssertEqual(events.value.count, 1)
        XCTAssertEqual(events.value.first?.type, "update")
        XCTAssertEqual(events.value.first?.sessionID, "session-1")
        XCTAssertEqual(events.value.first?.agent?.provider, "codex")
        XCTAssertEqual(events.value.first?.agent?.state, "running")
    }

    func testTimeoutAndCloseFailPendingRequests() async throws {
        let api = RemoteAPI(sender: { _ in }, onEvent: { _ in })
        do {
            _ = try await api.request(.health, timeout: 0.05)
            XCTFail("expected timeout")
        } catch {
            XCTAssertEqual(error as? RemoteAPIError, .timeout)
        }

        let sent = expectation(description: "pending request sent")
        let closeAPI = RemoteAPI(sender: { _ in sent.fulfill() }, onEvent: { _ in })
        let pending = Task { try await closeAPI.request(.health, timeout: 10) }
        await fulfillment(of: [sent], timeout: 1)
        await closeAPI.close(error: .transport("closed"))
        do {
            _ = try await pending.value
            XCTFail("expected close error")
        } catch {
            XCTAssertEqual(error as? RemoteAPIError, .transport("closed"))
        }
    }

    private func json(_ object: [String: Any]) -> Data {
        try! JSONSerialization.data(withJSONObject: object)
    }

    private func waitUntil(_ condition: @escaping () -> Bool, attempts: Int = 200) async -> Bool {
        for _ in 0..<attempts {
            if condition() { return true }
            try? await Task.sleep(nanoseconds: 5_000_000)
        }
        return condition()
    }
}

/// Small lock-protected box for values captured by `@Sendable` closures.
final class Box<Value>: @unchecked Sendable {
    private let lock = NSLock()
    private var storage: Value

    init(_ value: Value) {
        self.storage = value
    }

    var value: Value {
        lock.lock()
        defer { lock.unlock() }
        return storage
    }

    func modify(_ body: (inout Value) -> Void) {
        lock.lock()
        defer { lock.unlock() }
        body(&storage)
    }
}
