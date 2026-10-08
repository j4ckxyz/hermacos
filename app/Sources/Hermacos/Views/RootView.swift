import AppKit
import HermesCore
import SwiftUI

struct RootView: View {
    @Environment(AppModel.self) private var model

    var body: some View {
        Group {
            switch model.phase {
            case .launching:
                Color.clear
            case .signedOut:
                OnboardingView()
            case .ready:
                MainView()
            }
        }
        .background(WindowReader { model.mainWindow = $0 })
    }
}

/// Reports the window this view lives in.
private struct WindowReader: NSViewRepresentable {
    let found: (NSWindow?) -> Void

    func makeNSView(context: Context) -> NSView {
        let view = NSView()
        DispatchQueue.main.async { found(view.window) }
        return view
    }

    func updateNSView(_ view: NSView, context: Context) {
        DispatchQueue.main.async { found(view.window) }
    }
}

struct MainView: View {
    @Environment(AppModel.self) private var model
    @State private var columns: NavigationSplitViewVisibility = .all

    var body: some View {
        @Bindable var model = model
        NavigationSplitView(columnVisibility: $columns) {
            SidebarView()
                .navigationSplitViewColumnWidth(min: 220, ideal: 264, max: 360)
        } detail: {
            if model.selection == .status {
                StatusView()
            } else {
                ChatView(chat: model.chat)
                    .id(model.chat.id)
            }
        }
        .alert("Rename Chat", isPresented: .init(get: { model.renaming != nil }, set: { if !$0 { model.renaming = nil } })) {
            RenameFields(session: model.renaming)
        }
        .confirmationDialog(
            "Delete “\(model.pendingDelete?.displayTitle ?? "")”?",
            isPresented: .init(get: { model.pendingDelete != nil }, set: { if !$0 { model.pendingDelete = nil } })
        ) {
            Button("Delete", role: .destructive) {
                if let session = model.pendingDelete { model.delete(session) }
            }
        } message: {
            Text("This removes the conversation from Hermes on every device.")
        }
    }
}

private struct RenameFields: View {
    @Environment(AppModel.self) private var model
    let session: SessionSummary?
    @State private var title = ""

    var body: some View {
        TextField("Title", text: $title)
            .onAppear { title = session?.displayTitle ?? "" }
        Button("Rename") {
            if let session { model.rename(session, to: title) }
        }
        Button("Cancel", role: .cancel) {}
    }
}
