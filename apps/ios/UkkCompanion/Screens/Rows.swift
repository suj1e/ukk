import SwiftUI

struct WorkspaceRow: View {
    let workspace: Workspace

    var body: some View {
        VStack(alignment: .leading, spacing: 5) {
            HStack(spacing: 8) {
                Text(workspace.name)
                    .font(.headline)
                FixtureTag()
            }
            Text(workspace.hostLabel)
                .font(.footnote)
                .foregroundStyle(.secondary)
            Text("\(workspace.spaces.count) spaces")
                .font(.caption)
                .foregroundStyle(.secondary)
        }
        .padding(.vertical, 3)
    }
}

struct SpaceRow: View {
    let space: Space

    var body: some View {
        VStack(alignment: .leading, spacing: 5) {
            Text(space.name)
                .font(.headline)
            Text(space.projectRoot)
                .font(.caption.monospaced())
                .foregroundStyle(.secondary)
            Text("\(space.sessions.count) sessions")
                .font(.caption)
                .foregroundStyle(.secondary)
        }
        .padding(.vertical, 3)
    }
}

struct SessionRow: View {
    let session: Session

    private var agentSummary: String {
        session.agentRuns
            .map { "\($0.name) · \($0.status.label)" }
            .joined(separator: "  ")
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 5) {
            HStack(spacing: 8) {
                Text(session.title)
                    .font(.headline)
                Spacer(minLength: 4)
                StatusChip(text: session.lifecycle.label, tone: session.lifecycle.tone)
            }
            Text(agentSummary)
                .font(.caption)
                .foregroundStyle(.secondary)
                .lineLimit(2)
            Text("\(session.messages.count) fixture messages")
                .font(.caption2)
                .foregroundStyle(.secondary)
        }
        .padding(.vertical, 3)
    }
}
