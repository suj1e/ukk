import Foundation

// Phantom tags keep identifiers for different entities from being mixed up.
enum WorkspaceTag {}
enum SpaceTag {}
enum SessionTag {}
enum AgentRunTag {}
enum ChatMessageTag {}
enum DeviceTag {}

/// A stable, typed identifier. The raw UUID is opaque to the UI.
struct EntityID<Tag>: Hashable, Sendable, CustomStringConvertible {
    let rawValue: UUID

    init(rawValue: UUID = UUID()) {
        self.rawValue = rawValue
    }

    init?(string: String) {
        guard let value = UUID(uuidString: string) else { return nil }
        self.rawValue = value
    }

    var description: String { rawValue.uuidString }

    /// Deterministic identifier used only by the fixture catalogue.
    /// Fixtures must render identically on every launch.
    static func fixture(_ number: Int) -> EntityID<Tag> {
        let suffix = String(format: "%012d", number)
        guard let value = UUID(uuidString: "0A11CE00-0000-4000-8000-" + suffix) else {
            preconditionFailure("fixture identifier suffix is not a valid UUID tail")
        }
        return EntityID(rawValue: value)
    }
}

typealias WorkspaceID = EntityID<WorkspaceTag>
typealias SpaceID = EntityID<SpaceTag>
typealias SessionID = EntityID<SessionTag>
typealias AgentRunID = EntityID<AgentRunTag>
typealias ChatMessageID = EntityID<ChatMessageTag>
typealias DeviceID = EntityID<DeviceTag>
