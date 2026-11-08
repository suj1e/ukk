import SwiftUI

/// Agent detail: observation provenance, runtime binding and capability
/// negotiation. Never derives status from terminal text.
struct AgentScreen: View {
    @ObservedObject var model: AppModel
    let session: Session
    let agentRun: AgentRun

    private var agentMessages: [ChatMessage] {
        let scoped = session.messages(for: agentRun.id)
        return scoped.isEmpty && session.agentRuns.count == 1 ? session.messages : scoped
    }

    var body: some View {
        List {
            Section("Observation") {
                LabeledContent("Status") {
                    StatusChip(text: agentRun.status.label, tone: agentRun.status.tone)
                }
                LabeledContent("Reported by") {
                    Text(agentRun.statusSource)
                        .font(.caption)
                        .multilineTextAlignment(.trailing)
                }
                LabeledContent("Adapter") {
                    Text(agentRun.adapterKind)
                        .font(.caption.monospaced())
                }
                LabeledContent("Provenance") {
                    Text(agentRun.provenance.label)
                        .font(.caption.weight(.semibold))
                }
            }

            Section {
                LabeledContent("hc machine") {
                    Text(agentRun.terminalRef.machine)
                        .font(.caption.monospaced())
                }
                LabeledContent("hc terminal") {
                    Text(agentRun.terminalRef.terminal)
                        .font(.caption.monospaced())
                }
            } header: {
                Text("Runtime binding")
            } footer: {
                Text("A binding is metadata. Closing a Pane, Session or window never stops this runtime.")
            }

            Section {
                capabilityRow("Structured approvals", session.capabilities.structuredApprovals)
                capabilityRow("Send to Agent", session.capabilities.agentSend)
                capabilityRow("Terminal attach", session.capabilities.terminalAttach)
                capabilityRow("Screen preview", session.capabilities.screenPreview)
                capabilityRow("Screen control", session.capabilities.screenControl)
            } header: {
                Text("Grants for this device")
            } footer: {
                Text("Only grants reported by the Host are shown. Pairing grants nothing by itself.")
            }

            if !session.tools.isEmpty {
                Section("Provider tools") {
                    ForEach(session.tools) { tool in
                        VStack(alignment: .leading, spacing: 3) {
                            HStack {
                                Text(tool.name)
                                    .font(.callout.weight(.semibold))
                                Spacer()
                                Text(tool.status)
                                    .font(.caption)
                                    .foregroundStyle(tool.isError ? Color.red : Color.secondary)
                            }
                            if let input = tool.input {
                                Text(input)
                                    .font(.caption2.monospaced())
                                    .foregroundStyle(.secondary)
                                    .lineLimit(4)
                            }
                        }
                    }
                }
            }

            Section("Transcript for this agent") {
                if agentMessages.isEmpty {
                    Text("No provider messages for this Agent.")
                        .foregroundStyle(.secondary)
                } else {
                    ForEach(agentMessages) { message in
                        VStack(alignment: .leading, spacing: 4) {
                            Text(message.text)
                                .font(.callout)
                                .textSelection(.enabled)
                            MetadataLine(message: message)
                        }
                        .padding(.vertical, 2)
                    }
                }
            }

            Section {
                Text("Unknown is an observation, not proof the process stopped. Structured status is never inferred from terminal text.")
                    .font(.caption)
                    .foregroundStyle(.secondary)
            }
        }
        .listStyle(.insetGrouped)
        .navigationTitle(agentRun.name)
        .navigationBarTitleDisplayMode(.inline)
    }

    private func capabilityRow(_ title: String, _ isAvailable: Bool) -> some View {
        HStack {
            Text(title)
            Spacer()
            Text(isAvailable ? "Granted" : "Not granted")
                .font(.caption.weight(.semibold))
                .foregroundStyle(isAvailable ? Color.green : Color.secondary)
        }
    }
}
