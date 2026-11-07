import SwiftUI

/// Explicit unsupported approvals. There is no approve/reject affordance.
struct ApprovalUnsupportedCard: View {
    let state: ApprovalPanelState

    var body: some View {
        HStack(alignment: .top, spacing: 10) {
            Image(systemName: "hand.raised.slash")
                .foregroundStyle(Color.orange)
                .accessibilityHidden(true)
            VStack(alignment: .leading, spacing: 4) {
                Text(state.headline)
                    .font(.footnote.weight(.semibold))
                    .accessibilityIdentifier("approvalsUnsupportedHeadline")
                Text(state.detail)
                    .font(.caption)
                    .foregroundStyle(.secondary)
                Text("No approve or reject action is available in this build.")
                    .font(.caption2.weight(.semibold))
                    .foregroundStyle(Color.orange)
            }
            Spacer(minLength: 0)
        }
        .padding(10)
        .background(
            Color.orange.opacity(0.10),
            in: RoundedRectangle(cornerRadius: 12, style: .continuous)
        )
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier("approvalsUnsupported")
    }
}

/// Reserved Session overlay region for the M1 Remote Screen plugin.
struct RemoteScreenSlotCard: View {
    let state: SessionOverlayState

    var body: some View {
        HStack(spacing: 10) {
            Image(systemName: "rectangle.on.rectangle.angled")
                .foregroundStyle(.secondary)
                .accessibilityHidden(true)
            VStack(alignment: .leading, spacing: 2) {
                Text("\(state.title) · reserved slot")
                    .font(.caption.weight(.semibold))
                Text(state.detail)
                    .font(.caption2)
                    .foregroundStyle(.secondary)
                    .lineLimit(2)
            }
            Spacer(minLength: 0)
            Text("Unavailable")
                .font(.caption2.weight(.semibold))
                .foregroundStyle(.secondary)
        }
        .padding(10)
        .background(
            Color(.secondarySystemGroupedBackground).opacity(0.6),
            in: RoundedRectangle(cornerRadius: 12, style: .continuous)
        )
        .overlay(
            RoundedRectangle(cornerRadius: 12, style: .continuous)
                .strokeBorder(style: StrokeStyle(lineWidth: 1, dash: [4, 3]))
                .foregroundStyle(.tertiary)
        )
        .accessibilityElement(children: .combine)
        .accessibilityIdentifier("overlaySlotReserved")
        .accessibilityLabel("Remote Screen reserved slot. Not available in this build.")
    }
}

struct AgentChip: View {
    let agent: AgentRun

    var body: some View {
        HStack(spacing: 8) {
            Image(systemName: "terminal")
                .font(.caption)
                .foregroundStyle(.secondary)
                .accessibilityHidden(true)
            VStack(alignment: .leading, spacing: 2) {
                Text(agent.name)
                    .font(.caption.weight(.semibold))
                    .lineLimit(1)
                StatusChip(text: agent.status.label, tone: agent.status.tone)
            }
        }
        .padding(.horizontal, 10)
        .padding(.vertical, 8)
        .background(
            Color(.secondarySystemGroupedBackground),
            in: RoundedRectangle(cornerRadius: 12, style: .continuous)
        )
        .overlay(
            RoundedRectangle(cornerRadius: 12, style: .continuous)
                .strokeBorder(Color(.separator), lineWidth: 0.5)
        )
        .accessibilityElement(children: .combine)
        .accessibilityLabel("\(agent.name), \(agent.status.label). Fixture agent.")
    }
}
