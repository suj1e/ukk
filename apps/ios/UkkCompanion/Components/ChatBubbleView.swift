import SwiftUI

struct ChatBubbleView: View {
    let message: ChatMessage
    let agentName: String?

    var body: some View {
        Group {
            switch message.role {
            case .system:
                systemNote
            case .user:
                bubble(isUser: true)
            case .agent:
                bubble(isUser: false)
            }
        }
        .accessibilityIdentifier("messageRow-\(message.id)")
    }

    private func bubble(isUser: Bool) -> some View {
        HStack(alignment: .bottom, spacing: 0) {
            if isUser { Spacer(minLength: 40) }
            VStack(alignment: .leading, spacing: 4) {
                if !isUser, let agentName {
                    Text(agentName)
                        .font(.caption.weight(.semibold))
                        .foregroundStyle(.secondary)
                }
                Text(message.text)
                    .font(.callout)
                    .textSelection(.enabled)
                    .fixedSize(horizontal: false, vertical: true)
                MetadataLine(message: message)
            }
            .padding(10)
            .background(
                isUser
                    ? UkkTheme.accent.opacity(0.14)
                    : Color(.secondarySystemGroupedBackground),
                in: RoundedRectangle(cornerRadius: 14, style: .continuous)
            )
            if !isUser { Spacer(minLength: 40) }
        }
    }

    private var systemNote: some View {
        HStack {
            Spacer(minLength: 12)
            HStack(alignment: .top, spacing: 6) {
                Image(systemName: "info.circle")
                    .accessibilityHidden(true)
                Text(message.text)
                    .font(.caption)
                    .multilineTextAlignment(.leading)
                MetadataLine(message: message)
            }
            .foregroundStyle(.secondary)
            .padding(.horizontal, 10)
            .padding(.vertical, 7)
            .background(Color.orange.opacity(0.12), in: RoundedRectangle(cornerRadius: 10, style: .continuous))
            .accessibilityElement(children: .combine)
            .accessibilityLabel("Fixture notice. \(message.text)")
            Spacer(minLength: 12)
        }
    }
}
