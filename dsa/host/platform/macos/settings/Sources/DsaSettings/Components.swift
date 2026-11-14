import AppKit
import SwiftUI

/// A message on a page: what went wrong, what was done, or what to know.
struct Banner: View {
    enum Kind {
        case info, success, warning, error
    }

    let kind: Kind
    let title: String
    var message: String = ""

    private var symbol: (String, Color) {
        switch kind {
        case .info: return ("info.circle.fill", .accentColor)
        case .success: return ("checkmark.circle.fill", .green)
        case .warning: return ("exclamationmark.triangle.fill", .orange)
        case .error: return ("xmark.octagon.fill", .red)
        }
    }

    var body: some View {
        HStack(alignment: .firstTextBaseline, spacing: 8) {
            Image(systemName: symbol.0).foregroundStyle(symbol.1)
            VStack(alignment: .leading, spacing: 2) {
                if !title.isEmpty {
                    Text(title).fontWeight(.semibold)
                }
                if !message.isEmpty {
                    Text(message).textSelection(.enabled).fixedSize(horizontal: false, vertical: true)
                }
            }
        }
    }
}

/// A banner as a row of its own in a grouped form, shown only when `shown`.
struct BannerSection: View {
    let shown: Bool
    let kind: Banner.Kind
    let title: String
    var message: String = ""

    var body: some View {
        if shown {
            Section {
                Banner(kind: kind, title: title, message: message)
            }
        }
    }
}

/// A row's title with the line that explains it below, as System Settings shows them.
struct RowLabel: View {
    let title: String
    var detail: String = ""

    var body: some View {
        VStack(alignment: .leading, spacing: 2) {
            Text(title)
            if !detail.isEmpty {
                Text(detail)
                    .font(.callout)
                    .foregroundStyle(.secondary)
                    .fixedSize(horizontal: false, vertical: true)
            }
        }
    }
}

/// A short note in a page: a page's introduction, or what to know under a row. Every page
/// draws its notes with this, so they all look the same and wrap inside the form's width.
struct Note: View {
    let text: String
    var selectable = false

    var body: some View {
        if selectable {
            styled.textSelection(.enabled)
        } else {
            styled
        }
    }

    private var styled: some View {
        Text(text)
            .font(.callout)
            .foregroundStyle(.secondary)
            .fixedSize(horizontal: false, vertical: true)
    }
}

/// More about a row, folded away until the user opens it, so the row itself stays short.
struct LearnMore: View {
    let text: String

    var body: some View {
        DisclosureGroup {
            Note(text: text)
        } label: {
            Text(T("LearnMore.Label")).font(.callout)
        }
    }
}

/// A switch row. Only the user's change is sent; the switch shows what the service keeps
/// once it answers.
struct SwitchRow: View {
    let title: String
    var detail: String = ""
    let isOn: Bool
    var enabled = true
    let set: (Bool) async -> Void

    var body: some View {
        Toggle(isOn: Binding(get: { isOn }, set: { value in Task { await set(value) } })) {
            RowLabel(title: title, detail: detail)
        }
        .toggleStyle(.switch)
        .disabled(!enabled)
    }
}

/// A row with a button at its end.
struct ButtonRow: View {
    let title: String
    var detail: String = ""
    let action: String
    var enabled = true
    var busy = false
    let run: () -> Void

    var body: some View {
        HStack {
            RowLabel(title: title, detail: detail)
            Spacer()
            if busy {
                ProgressView().controlSize(.small)
            }
            Button(action, action: run).disabled(!enabled)
        }
    }
}

/// A row that opens something: a folder or Android's own settings.
struct LinkRow: View {
    let title: String
    var detail: String = ""
    var enabled = true
    let run: () -> Void

    var body: some View {
        Button(action: run) {
            HStack {
                RowLabel(title: title, detail: detail)
                Spacer()
                Image(systemName: "chevron.right").foregroundStyle(.tertiary)
            }
            .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .disabled(!enabled)
    }
}

/// A value row: a label and what the service reports.
struct ValueRow: View {
    let title: String
    var detail: String = ""
    let value: String

    var body: some View {
        LabeledContent {
            Text(value).textSelection(.enabled).multilineTextAlignment(.trailing)
        } label: {
            RowLabel(title: title, detail: detail)
        }
    }
}

/// A borderless copy button that puts `text` on the pasteboard and shows a check mark for a
/// moment, so the user sees that the copy happened.
struct CopyButton: View {
    let text: String
    @State private var copied = false

    var body: some View {
        Button {
            NSPasteboard.general.clearContents()
            NSPasteboard.general.setString(text, forType: .string)
            copied = true
            Task {
                try? await Task.sleep(nanoseconds: 1_500_000_000)
                copied = false
            }
        } label: {
            Image(systemName: copied ? "checkmark" : "doc.on.doc")
        }
        .buttonStyle(.borderless)
        .help(T("CopyButton.ToolTip"))
        .accessibilityLabel(T("CopyButton.ToolTip"))
    }
}

/// One command in monospaced, selectable text, with a button that copies exactly that command.
struct CommandRow: View {
    let command: String

    var body: some View {
        HStack {
            Text(command).font(.system(.body, design: .monospaced)).textSelection(.enabled)
            Spacer()
            CopyButton(text: command)
        }
    }
}

/// A picture from bytes Android sent (an app's icon, a widget's preview), or a stand-in.
struct PngImage: View {
    let data: Data?
    var size: CGFloat = 32
    var placeholder = "app.dashed"

    var body: some View {
        Group {
            if let data, let image = NSImage(data: data) {
                Image(nsImage: image).resizable().interpolation(.high).aspectRatio(contentMode: .fit)
            } else {
                Image(systemName: placeholder).resizable().aspectRatio(contentMode: .fit).foregroundStyle(.secondary).padding(4)
            }
        }
        .frame(width: size, height: size)
    }
}

extension View {
    /// The grouped form every page is, as System Settings draws its panes.
    func settingsPage() -> some View {
        formStyle(.grouped)
    }
}
