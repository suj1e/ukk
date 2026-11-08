import SwiftUI

/// Chat-first Session detail backed by Host RPC when a Desktop is paired.
/// Fixture sessions remain explicitly read-only.
struct SessionScreen: View {
    @ObservedObject var model: AppModel
    let session: Session

    @State private var draft = ""
    @State private var confirmAbort = false
    @State private var confirmStop = false

    private var composer: ComposerState {
        session.capabilities.composerState(isLive: model.connection.isLiveClaim)
    }

    private var approvalPanel: ApprovalPanelState {
        session.capabilities.approvalPanel(pendingCount: session.pendingApprovals.count)
    }

    private var isBusy: Bool {
        guard let rpcID = model.rpcID(for: session) else { return false }
        return model.busySessionID == rpcID
    }

    var body: some View {
        VStack(spacing: 0) {
            header
            if session.provenance == .live && !model.connection.isLiveClaim {
                staleNotice
            }
            if let error = model.sessionError {
                errorNotice(error)
            }
            ApprovalPanelCard(
                state: approvalPanel,
                approvals: session.approvals,
                isLive: model.connection.isLiveClaim,
                isBusy: isBusy
            ) { approval, approve in
                Task { await model.respond(to: approval, session: session, approve: approve) }
            }
            .padding(.horizontal, 16)
            .padding(.bottom, 8)
            if !session.tools.isEmpty {
                ToolCallsCard(tools: session.tools)
                    .padding(.horizontal, 16)
                    .padding(.bottom, 8)
            }
            messageList
        }
        .background(Color(.systemGroupedBackground))
        .safeAreaInset(edge: .bottom, spacing: 0) {
            bottomBar
        }
        .navigationTitle(session.title)
        .navigationBarTitleDisplayMode(.inline)
        .toolbar {
            ToolbarItem(placement: .topBarTrailing) {
                Menu {
                    Button {
                        Task { await model.refreshSession(session.rpcID ?? "") }
                    } label: {
                        Label("Refresh", systemImage: "arrow.clockwise")
                    }
                    .disabled(!model.connection.isLiveClaim)
                    Divider()
                    Button {
                        confirmAbort = true
                    } label: {
                        Label("Abort current turn", systemImage: "stop.circle")
                    }
                    .disabled(!model.connection.canDispatchCommands)
                    Button(role: .destructive) {
                        confirmStop = true
                    } label: {
                        Label("Stop Agent process", systemImage: "power")
                    }
                    .disabled(!model.connection.canDispatchCommands)
                } label: {
                    Label("Session actions", systemImage: "ellipsis.circle")
                }
                .accessibilityIdentifier("sessionActionsMenu")
            }
        }
        .confirmationDialog(
            "Abort the current turn?",
            isPresented: $confirmAbort,
            titleVisibility: .visible
        ) {
            Button("Abort turn", role: .destructive) {
                Task { await model.abort(session: session) }
            }
            Button("Cancel", role: .cancel) {}
        } message: {
            Text("This cancels only the in-flight turn. The Agent process keeps running.")
        }
        .confirmationDialog(
            "Stop the Agent process?",
            isPresented: $confirmStop,
            titleVisibility: .visible
        ) {
            Button("Stop process", role: .destructive) {
                Task { await model.stopAgent(session: session) }
            }
            Button("Cancel", role: .cancel) {}
        } message: {
            Text("This asks the Host to end the Agent process. Closing a view never does this on its own.")
        }
        .task {
            model.noteSessionOpened(session)
            if session.provenance == .live, let rpcID = session.rpcID {
                await model.refreshSession(rpcID)
            }
        }
    }

    private var header: some View {
        VStack(alignment: .leading, spacing: 8) {
            HStack(spacing: 6) {
                ConnectionStatusPill(model: model)
                StatusChip(text: session.lifecycle.label, tone: session.lifecycle.tone)
                ProvenanceTag(provenance: session.provenance)
                Spacer(minLength: 0)
            }
            if !session.agentRuns.isEmpty {
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
        }
        .padding(.horizontal, 16)
        .padding(.top, 10)
        .padding(.bottom, 8)
    }

    private var staleNotice: some View {
        HStack(spacing: 6) {
            Image(systemName: "exclamationmark.triangle.fill")
                .accessibilityHidden(true)
            Text("Showing last synced data. Not live.")
                .font(.caption.weight(.semibold))
            Spacer(minLength: 0)
        }
        .foregroundStyle(Color.orange)
        .padding(.horizontal, 16)
        .padding(.bottom, 8)
        .accessibilityIdentifier("sessionStaleNotice")
    }

    private func errorNotice(_ message: String) -> some View {
        Text(message)
            .font(.caption)
            .foregroundStyle(Color.red)
            .frame(maxWidth: .infinity, alignment: .leading)
            .padding(.horizontal, 16)
            .padding(.bottom, 8)
            .accessibilityIdentifier("sessionErrorNotice")
    }

    private var messageList: some View {
        ScrollViewReader { proxy in
            ScrollView {
                LazyVStack(alignment: .leading, spacing: 14) {
                    if session.messages.isEmpty {
                        Text(session.provenance == .live
                             ? "No messages in the current Host snapshot."
                             : "No fixture messages for this Session.")
                            .font(.footnote)
                            .foregroundStyle(.secondary)
                            .frame(maxWidth: .infinity, alignment: .center)
                            .padding(.top, 24)
                    }
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
            .onChange(of: session.messages.count) { _, _ in
                if let last = session.messages.last {
                    withAnimation { proxy.scrollTo(last.id, anchor: .bottom) }
                }
            }
            .onAppear {
                if let last = session.messages.last {
                    proxy.scrollTo(last.id, anchor: .bottom)
                }
            }
        }
    }

    private var bottomBar: some View {
        VStack(spacing: 8) {
            if session.provenance == .fixture {
                RemoteScreenSlotCard(state: .i1Reserved)
                    .padding(.horizontal, 16)
            } else if let rpcID = session.rpcID {
                RemoteScreenTile(
                    client: model.client,
                    sessionID: rpcID,
                    isConnectionLive: model.connection.isLiveClaim,
                    canRequestControl: session.capabilities.screenControl
                )
                .padding(.horizontal, 16)
            }

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

                Button {
                    let text = draft
                    Task {
                        if await model.sendPrompt(session: session, text: text) {
                            draft = ""
                        }
                    }
                } label: {
                    Image(systemName: "arrow.up.circle.fill")
                        .font(.title2)
                }
                .disabled(!composer.isEnabled || draft.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty || isBusy)
                .accessibilityIdentifier("composerSendButton")
            }
            .padding(.horizontal, 16)

            Text(composer.note)
                .font(.caption2)
                .foregroundStyle(.secondary)
                .frame(maxWidth: .infinity, alignment: .leading)
                .padding(.horizontal, 16)
                .accessibilityIdentifier("composerNote")
        }
        .padding(.vertical, 8)
        .background(.bar)
    }

    private func agentName(for message: ChatMessage) -> String? {
        guard let agentRunID = message.agentRunID else { return nil }
        return session.agentRun(agentRunID)?.name
    }
}
