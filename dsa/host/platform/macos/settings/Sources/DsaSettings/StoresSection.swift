import DsaSettingsCore
import SwiftUI

/// The alternative stores item, under Google Play: one row for each store with its plain name
/// (no logo, as each name is its owner's trademark), where it stands, and Install or Open.
struct StoresSection: View {
    @ObservedObject var vm: StoresViewModel

    var body: some View {
        Section(heading: T("AltStoresHeader.Text")) {
            Note(text: T("AltStoresCard.Description"))
            ForEach(vm.stores, id: \.id) { store in
                if store.installed {
                    ButtonRow(title: store.name, detail: vm.status(store), action: T("AltStoreOpenButton.Content"),
                              enabled: vm.canOpen(store)) {
                        Task { await vm.open(store.id) }
                    }
                } else {
                    ButtonRow(title: store.name, detail: vm.status(store), action: T("AltStoreInstallButton.Content"),
                              enabled: vm.canInstall(store), busy: vm.installing == store.id) {
                        Task { await vm.install(store.id) }
                    }
                }
            }
        }
        .task { await vm.load() }
    }
}
