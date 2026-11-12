import DsaSettingsCore
import SwiftUI

/// The protected content item: whether Widevine is installed, the buttons that take it from
/// Google's image (after the user agrees to Google's terms) and remove it, and what L3 means for
/// streaming.
struct WidevineSection: View {
    @ObservedObject var vm: WidevineViewModel

    var body: some View {
        Section(T("WidevineHeader.Text")) {
            if let error = vm.error {
                Banner(kind: .error, title: T("WidevineErrorBar.Title"), message: error)
            }
            ValueRow(title: T("WidevineStatusCard.Header"), detail: T("WidevineStatusCard.Description"),
                     value: vm.statusText)
            if vm.restartNeeded {
                Banner(kind: .info, title: T("WidevineRestartBar.Title"), message: T("WidevineRestartBar.Message"))
            }
            Text(vm.qualityText).font(.callout).foregroundStyle(.secondary)
                .fixedSize(horizontal: false, vertical: true)
            ButtonRow(title: T("WidevineInstallCard.Header"),
                      detail: vm.isInstalling ? T("WidevineInstalling.Text") : T("WidevineInstallCard.Description"),
                      action: T("WidevineInstallButton.Content"), enabled: vm.canInstall, busy: vm.isInstalling) {
                guard Shell.confirm(T("WidevineConfirm.Title"), vm.notice, T("WidevineConfirm.Action"),
                                    destructive: false) else {
                    return
                }
                Task { await vm.install() }
            }
            ButtonRow(title: T("WidevineRemoveCard.Header"), detail: T("WidevineRemoveCard.Description"),
                      action: T("WidevineRemoveButton.Content"), enabled: vm.canRemove,
                      busy: vm.isBusy && !vm.isInstalling) {
                Task { await vm.remove() }
            }
            if let terms = URL(string: vm.termsUrl) {
                Link(T("WidevineTermsLink.Text"), destination: terms).font(.callout)
            }
        }
        .task { await vm.load() }
    }
}
