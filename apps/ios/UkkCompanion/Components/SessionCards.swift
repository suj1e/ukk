import SwiftUI

/// Approval card. Unsupported states are explicit; a live provider approval can
/// only be decided through a confirmation dialog.
struct ApprovalPanelCard: View {
    let state: ApprovalPanelState
    let approvals: [ApprovalRequest]
    let isLive: Bool
    let isBusy: Bool
    let onDecision: (ApprovalRequest, Bool) -> Void

    @State private var pendingDecision: (approval: ApprovalRequest, approve: Bool)?

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            HStack(alignment: .top, spacing: 10) {
                Image(systemName: state.allowsDecision ? "hand.raised" : "hand.raised.slash")
                    .foregroundStyle(state.allowsDecision ? Color.orange : Color.secondary)
                    .accessibilityHidden(true)
                VStack(alignment: .leading, spacing: 4) {
                    Text(state.headline)
                        .font(.footnote.weight(.semibold))
                        .accessibilityIdentifier("approvalsHeadline")
                    Text(state.detail)
                        .font(.caption)
                        .foregroundStyle(.secondary)
                }
                Spacer(minLength: 0)
            }

            if state.allowsDecision {
                ForEach(approvals.filter(\.isPending)) { approval in
                    VStack(alignment: .leading, spacing: 4) {
                        Text(approval.toolName)
                            .font(.caption.weight(.semibold))
                        if let input = approval.input, !input.isEmpty {
                            Text(input)
                                .font(.caption2.monospaced())
                                .foregroundStyle(.secondary)
                                .lineLimit(4)
                        }
                        if let expires = approval.expiresAt {
                            Text("Expires \(expires, format: .dateTime.hour().minute())")
                                .font(.caption2)
                                .foregroundStyle(.secondary)
                        }
                        HStack(spacing: 8) {
                            Button("Approve") {
                                pendingDecision = (approval, true)
                            }
                            .buttonStyle(.borderedProminent)
                            .controlSize(.small)
                            .accessibilityIdentifier("approveButton-\(approval.id)")

                            Button("Deny", role: .destructive) {
                                pendingDecision = (approval, false)
                            }
                            .buttonStyle(.bordered)
                            .controlSize(.small)
                            .accessibilityIdentifier("denyButton-\(approval.id)")
                        }
                        .disabled(!isLive || isBusy)
                    }
                    .padding(.top, 2)
                }
            }
        }
        .padding(10)
        .background(
            Color.orange.opacity(state.allowsDecision ? 0.12 : 0.06),
            in: RoundedRectangle(cornerRadius: 12, style: .continuous)
        )
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier("approvalsPanel")
        .confirmationDialog(
            pendingDecision?.approve == true ? "Send approval?" : "Deny this request?",
            isPresented: Binding(
                get: { pendingDecision != nil },
                set: { if !$0 { pendingDecision = nil } }
            ),
            titleVisibility: .visible
        ) {
            if let decision = pendingDecision {
                Button(decision.approve ? "Approve and send" : "Deny and send", role: decision.approve ? nil : .destructive) {
                    onDecision(decision.approval, decision.approve)
                    pendingDecision = nil
                }
                Button("Cancel", role: .cancel) { pendingDecision = nil }
            }
        } message: {
            if let decision = pendingDecision {
                Text(decision.approve
                     ? "This tells the Host to approve \(decision.approval.toolName)."
                     : "This tells the Host to deny \(decision.approval.toolName).")
            }
        }
    }
}

/// Fixture-only reserved region for the Remote Screen plugin.
struct RemoteScreenSlotCard: View {
    let state: SessionOverlayState

    var body: some View {
        HStack(spacing: 10) {
            Image(systemName: "rectangle.on.rectangle.angled")
                .foregroundStyle(.secondary)
                .accessibilityHidden(true)
            VStack(alignment: .leading, spacing: 2) {
                Text("\(state.title) · unavailable")
                    .font(.caption.weight(.semibold))
                Text(state.detail)
                    .font(.caption2)
                    .foregroundStyle(.secondary)
                    .lineLimit(2)
            }
            Spacer(minLength: 0)
            Text("No Host")
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
        .accessibilityLabel("Remote Screen unavailable. No Host is paired.")
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
        .accessibilityLabel("\(agent.name), \(agent.status.label)")
    }
}

/// Compact view of provider-emitted tool calls.
struct ToolCallsCard: View {
    let tools: [ToolCall]

    var body: some View {
        VStack(alignment: .leading, spacing: 6) {
            Text("Provider tool calls")
                .font(.caption.weight(.semibold))
            ForEach(tools.suffix(4)) { tool in
                HStack(spacing: 6) {
                    Image(systemName: tool.isError ? "xmark.octagon" : "wrench.and.screwdriver")
                        .font(.caption2)
                        .foregroundStyle(tool.isError ? Color.red : Color.secondary)
                        .accessibilityHidden(true)
                    Text(tool.name)
                        .font(.caption2.weight(.semibold))
                    Text(tool.status)
                        .font(.caption2)
                        .foregroundStyle(.secondary)
                    Spacer(minLength: 0)
                }
            }
        }
        .padding(10)
        .background(
            Color(.secondarySystemGroupedBackground),
            in: RoundedRectangle(cornerRadius: 12, style: .continuous)
        )
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier("toolCallsCard")
    }
}
