// macOS 真机验收用独立窗口与真实修饰键事件。只截图本夹具，不读取用户现有窗口内容。
import AppKit
import ApplicationServices

let app = NSApplication.shared
app.setActivationPolicy(.regular)
let window = NSWindow(
    contentRect: NSRect(x: 240, y: 240, width: 720, height: 420),
    styleMask: [.titled, .closable], backing: .buffered, defer: false)
window.title = "Magi Appshot acceptance"
let label = NSTextField(labelWithString: "Magi Appshot acceptance\nNative window → plugin → daemon → composer")
label.frame = NSRect(x: 36, y: 160, width: 650, height: 100)
label.font = .systemFont(ofSize: 24)
label.maximumNumberOfLines = 3
window.contentView?.addSubview(label)
window.makeKeyAndOrderFront(nil)
app.activate(ignoringOtherApps: true)

DispatchQueue.main.asyncAfter(deadline: .now() + 1.5) {
    let source = CGEventSource(stateID: .hidSystemState)
    // 原生左右 Command 状态机读取 NX_DEVICELCMDKEYMASK / NX_DEVICERCMDKEYMASK。
    // 只设置公共 maskCommand 会被视为单键，不能冒充真实左右键触发。
    let flags: [UInt64] = [CGEventFlags.maskCommand.rawValue | 0x08,
                           CGEventFlags.maskCommand.rawValue | 0x18,
                           CGEventFlags.maskCommand.rawValue | 0x08, 0]
    for (index, key) in [55, 54, 54, 55].enumerated() {
        let event = CGEvent(keyboardEventSource: source, virtualKey: CGKeyCode(key), keyDown: index < 2)!
        event.type = .flagsChanged
        event.flags = CGEventFlags(rawValue: flags[index])
        event.post(tap: .cghidEventTap)
        usleep(120_000)
    }
}
DispatchQueue.main.asyncAfter(deadline: .now() + 6) { app.terminate(nil) }
app.run()
