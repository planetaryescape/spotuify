import SwiftUI
import SpotuifyKit

/// A compact device picker. Tapping a device transfers playback to it.
struct DeviceMenu: View {
    @Environment(AppModel.self) private var model
    var showsActiveName = true

    var body: some View {
        Menu {
            if model.player.devices.isEmpty {
                Text("No devices found")
            }
            ForEach(model.player.devices) { device in
                Button {
                    model.transfer(to: device)
                } label: {
                    Label(
                        device.name,
                        systemImage: device.isActive ? "checkmark" : DeviceIcon.symbol(for: device.kind)
                    )
                }
            }
        } label: {
            label
        }
        .menuStyle(.borderlessButton)
        .fixedSize()
        .foregroundStyle(model.player.activeDevice != nil ? AnyShapeStyle(.tint) : AnyShapeStyle(.secondary))
        .help(helpText)
        .accessibilityLabel(helpText)
    }

    private var activeName: String {
        model.player.activeDevice?.name ?? "No device"
    }

    @ViewBuilder
    private var label: some View {
        if showsActiveName {
            Label(activeName, systemImage: "hifispeaker.2.fill")
                .font(.caption)
                .lineLimit(1)
        } else {
            Image(systemName: "hifispeaker.2.fill")
                .font(.system(size: 13, weight: .semibold))
                .frame(width: 32, height: 32)
                .contentShape(Circle())
        }
    }

    private var helpText: String {
        model.player.activeDevice == nil ? "Change playback device" : "Change playback device: \(activeName)"
    }
}
