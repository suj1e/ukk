import Foundation

/// A typed navigation destination. The stack is explicit so navigation state is
/// unit-testable without any SwiftUI runtime.
enum AppRoute: Hashable, Sendable {
    case workspace(WorkspaceID)
    case space(SpaceID)
    case session(SessionID)
    case agentRun(AgentRunID)
}

struct NavigationState: Equatable, Sendable {
    var path: [AppRoute] = []

    var depth: Int { path.count }

    mutating func openWorkspace(_ id: WorkspaceID) {
        path = [.workspace(id)]
    }

    mutating func openSpace(_ id: SpaceID) {
        push(.space(id))
    }

    mutating func openSession(_ id: SessionID) {
        push(.session(id))
    }

    mutating func openAgentRun(_ id: AgentRunID) {
        push(.agentRun(id))
    }

    mutating func popToRoot() {
        path.removeAll()
    }

    var selectedWorkspaceID: WorkspaceID? {
        for route in path.reversed() {
            if case .workspace(let id) = route { return id }
        }
        return nil
    }

    var selectedSpaceID: SpaceID? {
        for route in path.reversed() {
            if case .space(let id) = route { return id }
        }
        return nil
    }

    var selectedSessionID: SessionID? {
        for route in path.reversed() {
            if case .session(let id) = route { return id }
        }
        return nil
    }

    var selectedAgentRunID: AgentRunID? {
        for route in path.reversed() {
            if case .agentRun(let id) = route { return id }
        }
        return nil
    }

    /// Pushing the route that is already on top is a no-op, matching SwiftUI's
    /// NavigationStack behaviour for repeated taps on the same row.
    private mutating func push(_ route: AppRoute) {
        guard path.last != route else { return }
        path.append(route)
    }
}
