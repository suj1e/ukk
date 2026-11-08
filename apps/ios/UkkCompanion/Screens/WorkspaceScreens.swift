import SwiftUI

struct RootView: View {
    @ObservedObject var model: AppModel

    var body: some View {
        VStack(spacing: 0) {
            StatusBanner(model: model)
            NavigationStack(path: $model.navigation.path) {
                WorkspaceListView(model: model)
                    .navigationDestination(for: AppRoute.self) { route in
                        destination(for: route)
                    }
            }
        }
        .tint(UkkTheme.accent)
        .task { model.start() }
        .sheet(isPresented: $model.isShowingTrustedDevices) {
            NavigationStack {
                TrustedDevicesScreen(model: model)
                    .toolbar {
                        ToolbarItem(placement: .topBarTrailing) {
                            Button("Done") { model.isShowingTrustedDevices = false }
                                .accessibilityIdentifier("devicesDoneButton")
                        }
                    }
            }
            .tint(UkkTheme.accent)
        }
    }

    @ViewBuilder
    private func destination(for route: AppRoute) -> some View {
        switch route {
        case .workspace(let id):
            if let workspace = model.workspace(id) {
                WorkspaceDetailScreen(model: model, workspace: workspace)
            } else {
                MissingEntityView(title: "Workspace unavailable")
            }

        case .space(let id):
            if let space = model.space(id) {
                SpaceScreen(model: model, space: space)
            } else {
                MissingEntityView(title: "Space unavailable")
            }

        case .session(let id):
            if let session = model.session(id) {
                SessionScreen(model: model, session: session)
            } else {
                MissingEntityView(title: "Session unavailable")
            }

        case .agentRun(let id):
            if let agent = model.agentRun(id),
               let session = model.session(agent.sessionID) {
                AgentScreen(model: model, session: session, agentRun: agent)
            } else {
                MissingEntityView(title: "Agent unavailable")
            }
        }
    }
}

struct WorkspaceListView: View {
    @ObservedObject var model: AppModel

    var body: some View {
        List {
            Section {
                ConnectionStatusView(model: model)
                    .listRowInsets(EdgeInsets(top: 12, leading: 16, bottom: 12, trailing: 16))
                    .listRowBackground(Color.clear)
            }

            if model.workspaces.isEmpty {
                Section {
                    ContentUnavailableView(
                        "No Host data",
                        systemImage: "network.slash",
                        description: Text(
                            model.isFixtureMode
                                ? "Pair a Desktop to load Spaces and Sessions."
                                : "The authenticated workspace snapshot is not available. Reconnect to load it."
                        )
                    )
                    .accessibilityIdentifier("workspaceUnavailable")
                }
            } else {
                Section {
                    ForEach(model.workspaces) { workspace in
                        NavigationLink(value: AppRoute.workspace(workspace.id)) {
                            WorkspaceRow(workspace: workspace)
                        }
                        .accessibilityIdentifier("workspaceRow-\(workspace.id)")
                    }
                } header: {
                    Text("Workspaces")
                } footer: {
                    if model.isFixtureMode {
                        Text(FixtureCatalog.disclaimer)
                    } else {
                        Text("Loaded from the paired Host. Rows are stale while the connection is not live.")
                    }
                }
            }
        }
        .listStyle(.insetGrouped)
        .navigationTitle("ukk")
        .toolbar {
            ToolbarItem(placement: .topBarTrailing) {
                Button {
                    model.isShowingTrustedDevices = true
                } label: {
                    Label("Devices", systemImage: "laptopcomputer.and.iphone")
                }
                .accessibilityIdentifier("trustedDevicesButton")
            }
        }
    }
}

struct WorkspaceDetailScreen: View {
    @ObservedObject var model: AppModel
    let workspace: Workspace

    var body: some View {
        List {
            Section {
                VStack(alignment: .leading, spacing: 6) {
                    HStack(spacing: 8) {
                        Text(workspace.name)
                            .font(.title3.weight(.semibold))
                        ProvenanceTag(provenance: workspace.provenance)
                    }
                    Text(workspace.hostLabel)
                        .font(.footnote)
                        .foregroundStyle(.secondary)
                }
                .padding(.vertical, 4)
            }

            Section {
                ForEach(workspace.spaces) { space in
                    NavigationLink(value: AppRoute.space(space.id)) {
                        SpaceRow(space: space)
                    }
                    .accessibilityIdentifier("spaceRow-\(space.id)")
                }
            } header: {
                Text("Spaces")
            } footer: {
                Text(
                    workspace.provenance == .fixture
                        ? "A Space is a project root on one Host. Fixture roots do not exist on this simulator."
                        : "A Space is a project root on the paired Desktop."
                )
            }
        }
        .listStyle(.insetGrouped)
        .navigationTitle(workspace.name)
        .navigationBarTitleDisplayMode(.inline)
    }
}

struct SpaceScreen: View {
    @ObservedObject var model: AppModel
    let space: Space

    var body: some View {
        List {
            Section {
                VStack(alignment: .leading, spacing: 6) {
                    Text(space.projectRoot)
                        .font(.footnote.monospaced())
                        .foregroundStyle(.secondary)
                    Text("\(space.sessions.count) sessions · \(space.provenance.label) data")
                        .font(.caption)
                        .foregroundStyle(.secondary)
                }
                .padding(.vertical, 4)
            }

            Section {
                ForEach(space.sessions) { session in
                    NavigationLink(value: AppRoute.session(session.id)) {
                        SessionRow(session: session)
                    }
                    .accessibilityIdentifier("sessionRow-\(session.id)")
                }
            } header: {
                Text("Sessions")
            } footer: {
                Text("Sessions are durable work contexts. Archiving or closing a view never stops an Agent process.")
            }
        }
        .listStyle(.insetGrouped)
        .navigationTitle(space.name)
        .navigationBarTitleDisplayMode(.inline)
    }
}
