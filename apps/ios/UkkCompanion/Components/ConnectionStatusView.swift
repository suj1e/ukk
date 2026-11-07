import SwiftUI

struct ConnectionStatusView: View {
    @ObservedObject var model: AppModel

    private var indicator: ConnectionIndicator { model.connection.indicator }

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            HStack(spacing: 8) {
                Image(systemName: indicator.systemImage)
                    .foregroundStyle(indicator.tone.color)
                    .accessibilityHidden(true)
                Text(indicator.label)
                    .font(.subheadline.weight(.semibold))
                    .accessibilityIdentifier("connectionStatusLabel")
                Spacer(minLength: 4)
                if model.connection.isRetryAvailable {
                    Button("Retry") { model.retryConnection() }
                        .buttonStyle(.bordered)
                        .controlSize(.small)
                        .accessibilityIdentifier("connectionRetryButton")
                } else if !indicator.isLive {
                    Text("Not live")
                        .font(.caption2.weight(.bold))
                        .foregroundStyle(.secondary)
                        .accessibilityIdentifier("notLiveBadge")
                }
            }
            Text(indicator.detail)
                .font(.footnote)
                .foregroundStyle(.secondary)
            if case .fixtureData = model.connection.state {
                Text("Live, reconnecting and offline indicators are wired to a unit-tested state machine, but this build has no transport.")
                    .font(.caption2)
                    .foregroundStyle(.secondary)
            }
        }
        .padding(12)
        .background(
            Color(.secondarySystemGroupedBackground),
            in: RoundedRectangle(cornerRadius: UkkTheme.cornerRadius, style: .continuous)
        )
        .accessibilityElement(children: .contain)
    }
}

/// Compact indicator used inside a Session screen.
struct ConnectionStatusPill: View {
    @ObservedObject var model: AppModel

    private var indicator: ConnectionIndicator { model.connection.indicator }

    var body: some View {
        HStack(spacing: 5) {
            Circle()
                .fill(indicator.tone.color)
                .frame(width: 7, height: 7)
                .accessibilityHidden(true)
            Text(indicator.label)
                .font(.caption2.weight(.semibold))
                .lineLimit(1)
                .accessibilityIdentifier("sessionConnectionStatus")
        }
        .padding(.horizontal, 8)
        .padding(.vertical, 4)
        .background(indicator.tone.color.opacity(0.12), in: Capsule())
    }
}
