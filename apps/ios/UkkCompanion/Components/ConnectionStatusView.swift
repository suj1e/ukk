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

            if let error = model.lastError {
                Text(error)
                    .font(.caption)
                    .foregroundStyle(Color.red)
                    .accessibilityIdentifier("hostErrorLabel")
            }

            if model.isFixtureMode {
                Text("Pair a Desktop to load real Sessions. Fixture rows are local samples and are never marked live.")
                    .font(.caption2)
                    .foregroundStyle(.secondary)
            } else if !indicator.isLive {
                Text("Cached Host data may be shown below. It is stale until a fresh authenticated session is online.")
                    .font(.caption2.weight(.semibold))
                    .foregroundStyle(Color.orange)
                    .accessibilityIdentifier("staleDataNotice")
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

/// Top-of-app honesty banner. Fixture mode is yellow and explicit; a degraded
/// or offline Host connection is orange/red. A live connection shows nothing.
struct StatusBanner: View {
    @ObservedObject var model: AppModel

    var body: some View {
        if model.isFixtureMode {
            FixtureBanner()
        } else {
            let indicator = model.connection.indicator
            if !indicator.isLive {
                HStack(spacing: 8) {
                    Image(systemName: indicator.systemImage)
                    Text(indicator.label.uppercased())
                        .accessibilityIdentifier("staleBannerLabel")
                    Spacer(minLength: 0)
                    Text("STALE DATA")
                        .font(.caption2.weight(.bold))
                }
                .font(.caption.weight(.semibold))
                .foregroundStyle(.white)
                .padding(.horizontal, 14)
                .padding(.vertical, 6)
                .frame(maxWidth: .infinity)
                .background(indicator.tone == .negative ? Color.red : Color.orange)
                .accessibilityElement(children: .contain)
                .accessibilityIdentifier("staleBanner")
            }
        }
    }
}
