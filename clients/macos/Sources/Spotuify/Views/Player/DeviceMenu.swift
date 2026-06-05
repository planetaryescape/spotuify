import SwiftUI
import SpotuifyKit

/// A compact device picker. Tapping a device transfers playback to it.
struct DeviceMenu: View {
    @Environment(AppModel.self) private var model

    var body: some View {
        Menu {
            if model.player.devices.isEmpty {
                Text("No devices found")
            }
            ForEach(model.player.devices) { device in
                Button {
                    model.transfer(to: device)
                } label: {
                    Label {
                        Text(device.name) + Text(device.isActive ? "  ✓" : "")
                    } icon: {
                        Image(systemName: deviceIcon(device.kind))
                    }
                }
            }
        } label: {
            Label(activeName, systemImage: "hifispeaker.2.fill")
                .font(.caption)
                .lineLimit(1)
        }
        .menuStyle(.borderlessButton)
        .fixedSize()
        .foregroundStyle(model.player.activeDevice != nil ? AnyShapeStyle(.tint) : AnyShapeStyle(.secondary))
    }

    private var activeName: String {
        model.player.activeDevice?.name ?? "No device"
    }

    private func deviceIcon(_ kind: String) -> String {
        switch kind.lowercased() {
        case "computer": "laptopcomputer"
        case "smartphone": "iphone"
        case "speaker": "hifispeaker.fill"
        case "tv", "castvideo": "tv"
        case "avr", "stb": "av.remote"
        default: "hifispeaker"
        }
    }
}
