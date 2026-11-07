import SwiftUI

struct RootView: View {
    @ObservedObject var model: AppModel

    var body: some View {
        VStack(spacing: 0) {
            FixtureBanner()
            NavigationStack(path: $model.navigation.path) {
                WorkspaceListView(model: model)
                    .navigationDestination(for: AppRoute.self) { route in
                        destination(for: route)
                    }
            }
        }
        .tint(UkkTheme.accent)
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
            if let workspace = model.catalog.workspace(id) {
                WorkspaceDetailScreen(model: model, workspace: workspace)
            } else {
                MissingEntityView(title: "Workspace unavailable")
            }

        case .space(let id):
            if let space = model.catalog.space(id) {
                SpaceScreen(model: model, space: space)
            } else {
                MissingEntityView(title: "Space unavailable")
            }

        case .session(let id):
            if let session = model.catalog.session(id) {
                SessionScreen(model: model, session: session)
            } else {
                MissingEntityView(title: "Session unavailable")
            }

        case .agentRun(let id):
            if let agent = model.catalog.agentRun(id),
               let session = model.catalog.session(agent.sessionID) {
                AgentScreen(session: session, agentRun: agent)
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

            Section {
                ForEach(model.catalog.workspaces) { workspace in
                    NavigationLink(value: AppRoute.workspace(workspace.id)) {
                        WorkspaceRow(workspace: workspace)
                    }
                    .accessibilityIdentifier("workspaceRow-\(workspace.id)")
                }
            } header: {
                Text("Workspaces")
            } footer: {
                Text(FixtureCatalog.disclaimer)
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
                        FixtureTag()
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
                Text("A Space is a project root on one Host. Fixture roots do not exist on this simulator.")
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
                    Text("\(space.sessions.count) sessions · fixture data")
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
