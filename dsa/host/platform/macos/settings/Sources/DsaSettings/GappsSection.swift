import DsaSettingsCore
import SwiftUI

/// The Google Play item: whether Google's apps are installed, the buttons that install and
/// remove them, and what to know about Google's terms and registering this device.
struct GappsSection: View {
    @ObservedObject var vm: GappsViewModel

    var body: some View {
        Section(T("GappsHeader.Text")) {
            if let error = vm.error {
                Banner(kind: .error, title: T("GappsErrorBar.Title"), message: error)
            }
            ValueRow(title: T("GappsStatusCard.Header"), detail: T("GappsStatusCard.Description"), value: vm.statusText)
            if vm.restartNeeded {
                Banner(kind: .info, title: T("GappsRestartBar.Title"), message: T("GappsRestartBar.Message"))
            }
            ButtonRow(title: T("GappsInstallCard.Header"),
                      detail: vm.isInstalling ? T("GappsInstalling.Text") : T("GappsInstallCard.Description"),
                      action: T("GappsInstallButton.Content"), enabled: vm.canInstall, busy: vm.isInstalling) {
                Task { await vm.install() }
            }
            ButtonRow(title: T("GappsRemoveCard.Header"), detail: T("GappsRemoveCard.Description"),
                      action: T("GappsRemoveButton.Content"), enabled: vm.canRemove, busy: vm.isBusy && !vm.isInstalling) {
                Task { await vm.remove() }
            }
            VStack(alignment: .leading, spacing: 4) {
                Text(T("GappsTerms.Text")).font(.callout).foregroundStyle(.secondary)
                    .fixedSize(horizontal: false, vertical: true)
                if let terms = URL(string: vm.termsUrl) {
                    Link(T("GappsTermsLink.Text"), destination: terms).font(.callout)
                }
            }
            VStack(alignment: .leading, spacing: 4) {
                RowLabel(title: T("GappsRegisterCard.Header"), detail: T("GappsRegisterCard.Description"))
                if let registration = URL(string: vm.registrationUrl) {
                    Link(vm.registrationUrl, destination: registration).font(.callout)
                }
            }
            if vm.hasDeviceId {
                LabeledContent {
                    Text(vm.deviceId).font(.system(.body, design: .monospaced)).textSelection(.enabled)
                } label: {
                    RowLabel(title: T("GappsDeviceIdCard.Header"), detail: T("GappsDeviceIdCard.Description"))
                }
            } else {
                Text(T("GappsDeviceIdUnknown.Text")).font(.callout).foregroundStyle(.secondary)
                    .fixedSize(horizontal: false, vertical: true)
            }
        }
        .task { await vm.load() }
    }
}
