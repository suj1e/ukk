import SwiftUI

enum UkkTheme {
    /// Muted slate blue, readable in light and dark.
    static let accent = Color(red: 0.16, green: 0.42, blue: 0.72)
    static let cornerRadius: CGFloat = 14
}

extension StatusTone {
    var color: Color {
        switch self {
        case .neutral: return .secondary
        case .positive: return .green
        case .warning: return .orange
        case .negative: return .red
        }
    }
}

extension ConnectionIndicator.Tone {
    var color: Color {
        switch self {
        case .neutral: return .secondary
        case .positive: return .green
        case .warning: return .orange
        case .negative: return .red
        }
    }
}

struct StatusChip: View {
    let text: String
    let tone: StatusTone

    var body: some View {
        Text(text)
            .font(.caption.weight(.semibold))
            .padding(.horizontal, 8)
            .padding(.vertical, 3)
            .foregroundStyle(tone.color)
            .background(tone.color.opacity(0.14), in: Capsule())
    }
}

struct FixtureTag: View {
    var body: some View {
        Label(FixtureCatalog.badgeLabel, systemImage: "testtube.2")
            .font(.caption2.weight(.bold))
            .padding(.horizontal, 7)
            .padding(.vertical, 3)
            .foregroundStyle(.primary)
            .background(Color.yellow.opacity(0.28), in: Capsule())
            .accessibilityLabel("Fixture data")
    }
}

/// Persistent, unmistakable marker that the whole app is local sample content.
struct FixtureBanner: View {
    var body: some View {
        HStack(spacing: 8) {
            Image(systemName: "testtube.2")
            Text(FixtureCatalog.bannerText)
                .accessibilityIdentifier("fixtureBannerLabel")
            Spacer(minLength: 0)
        }
        .font(.caption.weight(.semibold))
        .foregroundStyle(.black)
        .padding(.horizontal, 14)
        .padding(.vertical, 6)
        .frame(maxWidth: .infinity)
        .background(Color.yellow)
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier("fixtureBanner")
    }
}

struct MissingEntityView: View {
    let title: String

    var body: some View {
        ContentUnavailableView(
            title,
            systemImage: "questionmark.folder",
            description: Text("The requested fixture does not exist in this deterministic catalogue.")
        )
    }
}

struct MetadataLine: View {
    let message: ChatMessage

    var body: some View {
        HStack(spacing: 5) {
            Text(message.occurredAt, format: .dateTime.hour().minute())
            Text("·")
            Text(FixtureCatalog.badgeLabel.lowercased())
                .fontWeight(.semibold)
        }
        .font(.caption2)
        .foregroundStyle(.secondary)
    }
}
