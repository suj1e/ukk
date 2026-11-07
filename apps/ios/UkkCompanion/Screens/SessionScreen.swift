import SwiftUI

/// Chat-first Session detail. Messages are deterministic fixtures; sending and
/// approvals are explicitly unavailable rather than simulated.
struct SessionScreen: View {
    @ObservedObject var model: AppModel
    let session: Session

    @State private var draft = ""

    private var composer: ComposerState {
        session.capabilities.composerState
    }

    var body: some View {
        VStack(spacing: 0) {
            header
            ApprovalUnsupportedCard(state: session.capabilities.approvalPanel)
                .padding(.horizontal, 16)
                .padding(.bottom, 8)
            messageList
        }
        .background(Color(.systemGroupedBackground))
        .safeAreaInset(edge: .bottom, spacing: 0) {
            bottomBar
        }
        .navigationTitle(session.title)
        .navigationBarTitleDisplayMode(.inline)
    }

    private var header: some View {
        VStack(alignment: .leading, spacing: 8) {
            HStack(spacing: 6) {
                ConnectionStatusPill(model: model)
                StatusChip(text: session.lifecycle.label, tone: session.lifecycle.tone)
                FixtureTag()
                Spacer(minLength: 0)
            }
            ScrollView(.horizontal, showsIndicators: false) {
                HStack(spacing: 8) {
                    ForEach(session.agentRuns) { agent in
                        NavigationLink(value: AppRoute.agentRun(agent.id)) {
                            AgentChip(agent: agent)
                        }
                        .buttonStyle(.plain)
                        .accessibilityIdentifier("agentChip-\(agent.id)")
                    }
                }
                .padding(.vertical, 2)
            }
        }
        .padding(.horizontal, 16)
        .padding(.top, 10)
        .padding(.bottom, 8)
    }

    private var messageList: some View {
        ScrollViewReader { proxy in
            ScrollView {
                LazyVStack(alignment: .leading, spacing: 14) {
                    ForEach(session.messages) { message in
                        ChatBubbleView(
                            message: message,
                            agentName: agentName(for: message)
                        )
                        .id(message.id)
                    }
                }
                .padding(.horizontal, 16)
                .padding(.vertical, 12)
            }
            .accessibilityIdentifier("chatTranscript")
            .onAppear {
                if let last = session.messages.last {
                    proxy.scrollTo(last.id, anchor: .bottom)
                }
            }
        }
    }

    private var bottomBar: some View {
        VStack(spacing: 8) {
            RemoteScreenSlotCard(state: .i1Reserved)
                .padding(.horizontal, 16)

            HStack(spacing: 8) {
                TextField("Message", text: $draft, axis: .vertical)
                    .lineLimit(1...4)
                    .padding(10)
                    .background(
                        Color(.secondarySystemGroupedBackground),
                        in: RoundedRectangle(cornerRadius: 12, style: .continuous)
                    )
                    .disabled(!composer.isEnabled)
                    .accessibilityIdentifier("composerField")

                Button {} label: {
                    Image(systemName: "arrow.up.circle.fill")
                        .font(.title2)
                }
                .disabled(!composer.isEnabled)
                .accessibilityIdentifier("composerSendButton")
            }
            .padding(.horizontal, 16)

            Text(composer.note)
                .font(.caption2)
                .foregroundStyle(.secondary)
                .frame(maxWidth: .infinity, alignment: .leading)
                .padding(.horizontal, 16)
                .accessibilityIdentifier("composerDisabledNote")
        }
        .padding(.vertical, 8)
        .background(.bar)
    }

    private func agentName(for message: ChatMessage) -> String? {
        guard let agentRunID = message.agentRunID else { return nil }
        return session.agentRun(agentRunID)?.name
    }
}
