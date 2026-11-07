import SwiftUI

/// Agent detail: observation provenance, runtime binding and capability
/// negotiation. Never derives status from terminal text.
struct AgentScreen: View {
    let session: Session
    let agentRun: AgentRun

    private var agentMessages: [ChatMessage] {
        session.messages(for: agentRun.id)
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
                    FixtureTag()
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
                Text("Capabilities")
            } footer: {
                Text("Only capabilities negotiated with a Host are shown as available. This build negotiated nothing.")
            }

            Section("Transcript for this agent") {
                if agentMessages.isEmpty {
                    Text("No fixture messages for this AgentRun.")
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
            Text(isAvailable ? "Available" : "Unavailable")
                .font(.caption.weight(.semibold))
                .foregroundStyle(isAvailable ? Color.green : Color.secondary)
        }
    }
}
