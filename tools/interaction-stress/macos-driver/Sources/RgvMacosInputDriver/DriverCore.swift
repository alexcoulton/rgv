import AppKit
import ApplicationServices
import Foundation

let driverSchema = "rgv.macos_input_driver"
let driverSchemaVersion = 1
let maximumEvents = 100_000

enum DriverOperation: String, Codable {
    case preflight
    case inspectWindow = "inspect_window"
    case deliver
    case emergencyRelease = "emergency_release"
    case restorePointer = "restore_pointer"
}

enum DriverEventKind: String, Codable {
    case move
    case primaryDown = "primary_down"
    case primaryDrag = "primary_drag"
    case primaryUp = "primary_up"
    case secondaryDown = "secondary_down"
    case secondaryDrag = "secondary_drag"
    case secondaryUp = "secondary_up"
    case scroll
    case keyDown = "key_down"
    case keyUp = "key_up"
    case focus
    case resizeWindow = "resize_window"
}

enum ScrollUnit: String, Codable {
    case line
    case pixel
}

enum TouchPhase: String, Codable {
    case none
    case began
    case changed
    case ended
    case cancelled
    case mayBegin = "may_begin"
}

enum DriverPermission: String, Codable {
    case granted
    case denied
    case unknown
}

struct StressModifiers: Codable {
    var shift = false
    var control = false
    var option = false
    var command = false
}

struct CompiledDriverEvent: Codable {
    let sequence: UInt64
    let atMicros: UInt64
    let kind: DriverEventKind
    let point: [Double]?
    let deltaX: Double
    let deltaY: Double
    let scrollUnit: ScrollUnit
    let phase: TouchPhase
    let momentumPhase: TouchPhase
    let continuous: Bool
    let keyCode: UInt16?
    let text: String?
    let focused: Bool?
    let logicalSize: [UInt32]?
    let modifiers: StressModifiers
}

struct DriverRequest: Codable {
    let schema: String
    let schemaVersion: Int
    let requestId: String
    let operation: DriverOperation
    let applicationInstanceId: String
    let runId: String
    let stepId: String
    let processId: UInt32
    let expectedProcessName: String
    let expectedWindowTitlePrefix: String
    let expectedContentSize: [Double]
    var expectedWindowBounds: [Double]? = nil
    let maximumDurationMillis: UInt64
    let events: [CompiledDriverEvent]
    var pointer: [Double]? = nil

    func validate() throws {
        guard schema == driverSchema, schemaVersion == driverSchemaVersion else {
            throw DriverError.invalidRequest("unsupported driver schema")
        }
        guard processId > 0,
              !requestId.isEmpty,
              !applicationInstanceId.isEmpty,
              !runId.isEmpty,
              !stepId.isEmpty,
              !expectedProcessName.isEmpty else {
            throw DriverError.invalidRequest("process identity is incomplete")
        }
        guard expectedContentSize.count == 2,
              expectedContentSize.allSatisfy({ $0.isFinite && $0 > 0 }),
              maximumDurationMillis > 0 else {
            throw DriverError.invalidRequest("content geometry or duration is invalid")
        }
        if let expectedWindowBounds {
            guard expectedWindowBounds.count == 4,
                  expectedWindowBounds.allSatisfy(\.isFinite),
                  expectedWindowBounds[2] > 0,
                  expectedWindowBounds[3] > 0 else {
                throw DriverError.invalidRequest("expected window bounds are invalid")
            }
        }
        guard events.count <= maximumEvents else {
            throw DriverError.invalidRequest("event count exceeds the protocol bound")
        }
        if operation == .deliver && events.isEmpty {
            throw DriverError.invalidRequest("deliver requests require events")
        }
        if (operation == .preflight
            || operation == .inspectWindow
            || operation == .restorePointer) && !events.isEmpty {
            throw DriverError.invalidRequest("inspection requests cannot contain events")
        }
        if operation == .restorePointer {
            guard let pointer,
                  pointer.count == 2,
                  pointer.allSatisfy(\.isFinite) else {
                throw DriverError.invalidRequest("restore_pointer requires a finite point")
            }
        } else if pointer != nil {
            throw DriverError.invalidRequest("only restore_pointer accepts a pointer")
        }
        if operation == .emergencyRelease
            && events.contains(where: { $0.kind != .keyUp || $0.keyCode == nil }) {
            throw DriverError.invalidRequest(
                "emergency release requests may contain only explicit key releases"
            )
        }
        var previous: UInt64 = 0
        for (index, event) in events.enumerated() {
            if index > 0 && event.atMicros < previous {
                throw DriverError.invalidRequest("event timestamps are not monotonic")
            }
            previous = event.atMicros
            if event.atMicros / 1_000 > maximumDurationMillis {
                throw DriverError.invalidRequest("event schedule exceeds maximum duration")
            }
            if let point = event.point,
               point.count != 2 || point.contains(where: { !$0.isFinite }) {
                throw DriverError.invalidRequest("event point is invalid")
            }
            if !event.deltaX.isFinite || !event.deltaY.isFinite {
                throw DriverError.invalidRequest("scroll delta is invalid")
            }
        }
    }
}

/// The correlation field is decoded before the full request so malformed payloads can still
/// return an authenticated-by-correlation failure instead of an opaque `unknown` response.
struct DriverRequestIdentity: Decodable {
    let requestId: String
}

struct DriverWindow: Codable {
    let processId: UInt32
    let processName: String
    let title: String
    let globalBounds: [Double]
    let contentOrigin: [Double]
    let contentSize: [Double]
    let focused: Bool
}

struct DriverAcknowledgement: Codable {
    let sequence: UInt64
    let requestedAtMicros: UInt64
    let attemptedAtMicros: UInt64
    let succeeded: Bool
    let failure: String?
}

struct DriverResponse: Codable {
    let schema: String
    let schemaVersion: Int
    let requestId: String
    let succeeded: Bool
    let permission: DriverPermission
    let failure: String?
    let window: DriverWindow?
    let acknowledgements: [DriverAcknowledgement]
    let emergencyReleases: UInt64
    var pointerPosition: [Double]? = nil
}

enum DriverError: Error, CustomStringConvertible {
    case invalidArguments(String)
    case invalidRequest(String)
    case permissionDenied
    case processMissing
    case processMismatch(String)
    case windowMissing
    case windowAmbiguous
    case windowIdentityMissing
    case windowIdentityAmbiguous
    case windowNotFocused
    case windowGeometryChanged
    case unsafeGlobalDelivery(String)
    case accessibility(String)
    case eventCreation(String)

    var description: String {
        switch self {
        case .invalidArguments(let value), .invalidRequest(let value),
             .processMismatch(let value), .accessibility(let value),
             .eventCreation(let value):
            return value
        case .unsafeGlobalDelivery(let value):
            return "refusing global input: \(value)"
        case .permissionDenied:
            return "macOS Accessibility/Input Monitoring permission is not granted"
        case .processMissing:
            return "the requested process is no longer running"
        case .windowMissing:
            return "the requested process has no matching visible window"
        case .windowAmbiguous:
            return "more than one window matched the requested RGV identity"
        case .windowIdentityMissing:
            return "the selected RGV window has no matching WindowServer identity"
        case .windowIdentityAmbiguous:
            return "more than one WindowServer window matched the selected RGV window"
        case .windowNotFocused:
            return "the selected RGV window lost focus outside a scheduled focus step"
        case .windowGeometryChanged:
            return "the selected RGV window moved or resized outside the requested operation"
        }
    }
}

struct WindowHandle {
    let element: AXUIElement
    let snapshot: DriverWindow
    let windowId: CGWindowID
    let chromeWidth: Double
    let chromeHeight: Double
}

@MainActor final class FocusSentinel {
    private let application: NSApplication
    private let window: NSWindow

    init() {
        application = NSApplication.shared
        application.setActivationPolicy(.accessory)
        application.finishLaunching()
        window = NSWindow(
            contentRect: NSRect(x: 0, y: 0, width: 1, height: 1),
            styleMask: [.borderless],
            backing: .buffered,
            defer: false
        )
        window.title = "RGV interaction-stress focus sentinel"
        window.isReleasedWhenClosed = false
        window.collectionBehavior = [.canJoinAllSpaces, .stationary, .ignoresCycle]
    }

    func takeFocus() throws {
        window.makeKeyAndOrderFront(nil)
        application.activate(ignoringOtherApps: true)
        let pid = getpid()
        let deadline = DispatchTime.now().uptimeNanoseconds + 2_000_000_000
        while NSWorkspace.shared.frontmostApplication?.processIdentifier != pid {
            guard DispatchTime.now().uptimeNanoseconds < deadline else {
                throw DriverError.windowNotFocused
            }
            RunLoop.current.run(until: Date(timeIntervalSinceNow: 0.01))
        }
    }

    func releaseFocus() {
        window.orderOut(nil)
        application.deactivate()
    }

    func yieldFocus(to target: NSRunningApplication) {
        if #available(macOS 14.0, *) {
            application.yieldActivation(to: target)
            target.activate(
                from: NSRunningApplication.current,
                options: [.activateAllWindows]
            )
            window.orderOut(nil)
        } else {
            releaseFocus()
            target.activate(options: [.activateAllWindows, .activateIgnoringOtherApps])
        }
    }

}

struct WindowServerWindow: Equatable {
    let id: CGWindowID
    let processId: pid_t
    let layer: Int
    let bounds: CGRect
    let title: String?
}

struct DriverArguments {
    let request: URL
    let response: URL

    static func parse(_ arguments: [String]) throws -> DriverArguments {
        var request: URL?
        var response: URL?
        var index = 1
        while index < arguments.count {
            guard index + 1 < arguments.count else {
                throw DriverError.invalidArguments("driver options require values")
            }
            switch arguments[index] {
            case "--request":
                request = URL(fileURLWithPath: arguments[index + 1])
            case "--response":
                response = URL(fileURLWithPath: arguments[index + 1])
            default:
                throw DriverError.invalidArguments("unknown driver option \(arguments[index])")
            }
            index += 2
        }
        guard let request, let response else {
            throw DriverError.invalidArguments("--request and --response are required")
        }
        return DriverArguments(request: request, response: response)
    }
}

func decoder() -> JSONDecoder {
    let decoder = JSONDecoder()
    decoder.keyDecodingStrategy = .convertFromSnakeCase
    return decoder
}

func encoder() -> JSONEncoder {
    let encoder = JSONEncoder()
    encoder.keyEncodingStrategy = .convertToSnakeCase
    encoder.outputFormatting = [.prettyPrinted, .sortedKeys]
    return encoder
}

func permissionState() -> DriverPermission {
    if AXIsProcessTrusted() && CGPreflightPostEventAccess() {
        return .granted
    }
    return .denied
}

func inspectWindow(
    request: DriverRequest,
    contentSize: [Double]? = nil,
    allowHidden: Bool = false,
    expectedBounds: [Double]? = nil,
    enforceRequestBounds: Bool = true
) throws -> WindowHandle {
    let pid = pid_t(request.processId)
    guard let application = NSRunningApplication(processIdentifier: pid), !application.isTerminated else {
        throw DriverError.processMissing
    }
    let observedName = application.localizedName ?? application.bundleIdentifier ?? ""
    let expected = request.expectedProcessName.lowercased()
    if !observedName.lowercased().contains(expected)
        && !expected.contains(observedName.lowercased()) {
        throw DriverError.processMismatch(
            "process name mismatch: expected \(request.expectedProcessName), observed \(observedName)"
        )
    }
    let applicationElement = AXUIElementCreateApplication(pid)
    let windows: [AXUIElement] = try attribute(applicationElement, kAXWindowsAttribute)
    let candidates = windows.compactMap { window -> (AXUIElement, String)? in
        let title: String = (try? attribute(window, kAXTitleAttribute)) ?? ""
        let role: String = (try? attribute(window, kAXRoleAttribute)) ?? ""
        let visible = !application.isHidden || allowHidden
        guard visible,
              role == (kAXWindowRole as String),
              title.hasPrefix(request.expectedWindowTitlePrefix) else {
            return nil
        }
        return (window, title)
    }
    guard !candidates.isEmpty else { throw DriverError.windowMissing }
    guard candidates.count == 1 else { throw DriverError.windowAmbiguous }
    let (window, title) = candidates[0]
    let position: CGPoint = try axPoint(window, kAXPositionAttribute)
    let size: CGSize = try axSize(window, kAXSizeAttribute)
    let observedBounds = [
        Double(position.x), Double(position.y), Double(size.width), Double(size.height),
    ]
    let requiredBounds = expectedBounds
        ?? (enforceRequestBounds ? request.expectedWindowBounds : nil)
    if let requiredBounds,
       zip(observedBounds, requiredBounds).contains(where: { abs($0 - $1) > 2.0 }) {
        throw DriverError.windowGeometryChanged
    }
    let desiredContent = contentSize ?? request.expectedContentSize
    let chrome = try validatedChromeDimensions(windowSize: size, contentSize: desiredContent)
    let chromeWidth = chrome.width
    let chromeHeight = chrome.height
    let contentOrigin = [
        Double(position.x) + chromeWidth / 2,
        Double(position.y) + chromeHeight,
    ]
    let focused: Bool = (try? attribute(window, kAXFocusedAttribute)) ?? false
    let windowId = try selectWindowServerWindowId(
        windows: windowServerWindows(),
        processId: pid,
        expectedBounds: CGRect(
            x: position.x,
            y: position.y,
            width: size.width,
            height: size.height
        ),
        expectedTitle: title
    )
    return WindowHandle(
        element: window,
        snapshot: DriverWindow(
            processId: request.processId,
            processName: observedName,
            title: title,
            globalBounds: observedBounds,
            contentOrigin: contentOrigin,
            contentSize: desiredContent,
            focused: focused
        ),
        windowId: windowId,
        chromeWidth: chromeWidth,
        chromeHeight: chromeHeight
    )
}

func validatedChromeDimensions(
    windowSize: CGSize,
    contentSize: [Double]
) throws -> (width: Double, height: Double) {
    guard contentSize.count == 2 else { throw DriverError.windowGeometryChanged }
    let rawWidth = Double(windowSize.width) - contentSize[0]
    let rawHeight = Double(windowSize.height) - contentSize[1]
    guard rawWidth >= -2, rawHeight >= -2, rawWidth <= 32, rawHeight <= 160 else {
        throw DriverError.windowGeometryChanged
    }
    return (max(0, rawWidth), max(0, rawHeight))
}

func windowServerWindows(
    options: CGWindowListOption = [.optionAll, .excludeDesktopElements]
) -> [WindowServerWindow] {
    guard let dictionaries = CGWindowListCopyWindowInfo(
        options,
        kCGNullWindowID
    ) as? [[String: Any]] else {
        return []
    }
    return dictionaries.compactMap { dictionary in
        guard let id = (dictionary[kCGWindowNumber as String] as? NSNumber)?.uint32Value,
              let processId = (dictionary[kCGWindowOwnerPID as String] as? NSNumber)?.int32Value,
              let layer = (dictionary[kCGWindowLayer as String] as? NSNumber)?.intValue,
              let boundsDictionary = dictionary[kCGWindowBounds as String] as? NSDictionary else {
            return nil
        }
        let alpha = (dictionary[kCGWindowAlpha as String] as? NSNumber)?.doubleValue ?? 1
        guard alpha > 0 else { return nil }
        var bounds = CGRect.zero
        guard CGRectMakeWithDictionaryRepresentation(boundsDictionary, &bounds) else {
            return nil
        }
        return WindowServerWindow(
            id: id,
            processId: processId,
            layer: layer,
            bounds: bounds,
            title: dictionary[kCGWindowName as String] as? String
        )
    }
}

func selectWindowServerWindowId(
    windows: [WindowServerWindow],
    processId: pid_t,
    expectedBounds: CGRect,
    expectedTitle: String
) throws -> CGWindowID {
    let matches = windows.filter { window in
        window.processId == processId
            && window.layer == 0
            && rectanglesApproximatelyEqual(window.bounds, expectedBounds)
    }
    guard !matches.isEmpty else { throw DriverError.windowIdentityMissing }
    if matches.count == 1 {
        return matches[0].id
    }
    let titleMatches = matches.filter { $0.title == expectedTitle }
    guard titleMatches.count == 1 else { throw DriverError.windowIdentityAmbiguous }
    return titleMatches[0].id
}

func rectanglesApproximatelyEqual(_ lhs: CGRect, _ rhs: CGRect) -> Bool {
    abs(lhs.origin.x - rhs.origin.x) <= 2.0
        && abs(lhs.origin.y - rhs.origin.y) <= 2.0
        && abs(lhs.size.width - rhs.size.width) <= 2.0
        && abs(lhs.size.height - rhs.size.height) <= 2.0
}

func frontmostNormalWindow(
    at point: CGPoint,
    windows: [WindowServerWindow]
) -> WindowServerWindow? {
    windows.first { $0.layer == 0 && $0.bounds.contains(point) }
}

func validateGlobalPointerRouting(
    expectedProcessId: pid_t,
    expectedWindowId: CGWindowID,
    expectedWindowBounds: CGRect,
    expectedContentBounds: CGRect,
    point: CGPoint,
    frontmostProcessId: pid_t?,
    windows: [WindowServerWindow]
) throws {
    guard expectedContentBounds.contains(point) else {
        throw DriverError.unsafeGlobalDelivery("target point is outside the validated RGV content")
    }
    guard frontmostProcessId == expectedProcessId else {
        throw DriverError.unsafeGlobalDelivery("RGV is no longer the frontmost application")
    }
    guard let hit = frontmostNormalWindow(at: point, windows: windows) else {
        throw DriverError.unsafeGlobalDelivery("no normal window owns the target point")
    }
    guard hit.processId == expectedProcessId, hit.id == expectedWindowId else {
        throw DriverError.unsafeGlobalDelivery(
            "another window owns the target point "
                + "(expected pid \(expectedProcessId) window \(expectedWindowId), "
                + "observed pid \(hit.processId) window \(hit.id) layer \(hit.layer) "
                + "title \(hit.title ?? "<untitled>"))"
        )
    }
    guard rectanglesApproximatelyEqual(hit.bounds, expectedWindowBounds) else {
        throw DriverError.unsafeGlobalDelivery("the RGV window moved or resized before delivery")
    }
}

func validateGlobalPointerRouting(
    pid: pid_t,
    windowId: CGWindowID,
    windowBounds: CGRect,
    contentBounds: CGRect,
    point: CGPoint
) throws {
    let frontmostPid = NSWorkspace.shared.frontmostApplication?.processIdentifier
    let visibleWindows = windowServerWindows(options: [.optionOnScreenOnly, .excludeDesktopElements])
    try validateGlobalPointerRouting(
        expectedProcessId: pid,
        expectedWindowId: windowId,
        expectedWindowBounds: windowBounds,
        expectedContentBounds: contentBounds,
        point: point,
        frontmostProcessId: frontmostPid,
        windows: visibleWindows
    )
}

func attribute<T>(_ element: AXUIElement, _ name: String) throws -> T {
    var value: CFTypeRef?
    let result = AXUIElementCopyAttributeValue(element, name as CFString, &value)
    guard result == .success, let typed = value as? T else {
        throw DriverError.accessibility("could not read Accessibility attribute \(name): \(result.rawValue)")
    }
    return typed
}

func axPoint(_ element: AXUIElement, _ name: String) throws -> CGPoint {
    let value: AXValue = try attribute(element, name)
    var point = CGPoint.zero
    guard AXValueGetValue(value, .cgPoint, &point) else {
        throw DriverError.accessibility("Accessibility attribute \(name) was not a point")
    }
    return point
}

func axSize(_ element: AXUIElement, _ name: String) throws -> CGSize {
    let value: AXValue = try attribute(element, name)
    var size = CGSize.zero
    guard AXValueGetValue(value, .cgSize, &size) else {
        throw DriverError.accessibility("Accessibility attribute \(name) was not a size")
    }
    return size
}

func eventFlags(_ modifiers: StressModifiers) -> CGEventFlags {
    var flags: CGEventFlags = []
    if modifiers.shift { flags.insert(.maskShift) }
    if modifiers.control { flags.insert(.maskControl) }
    if modifiers.option { flags.insert(.maskAlternate) }
    if modifiers.command { flags.insert(.maskCommand) }
    return flags
}

func phaseValue(_ phase: TouchPhase) -> Int64 {
    switch phase {
    case .none: return 0
    case .began: return 1
    case .changed: return 2
    case .ended: return 4
    case .cancelled: return 8
    case .mayBegin: return 128
    }
}

func makeGlobalMouseEvent(
    source: CGEventSource?,
    type: CGEventType,
    point: CGPoint,
    button: CGMouseButton,
    flags: CGEventFlags,
    eventNumber: Int64
) throws -> CGEvent {
    guard let event = CGEvent(
        mouseEventSource: source,
        mouseType: type,
        mouseCursorPosition: point,
        mouseButton: button
    ) else {
        throw DriverError.eventCreation("could not create mouse event")
    }
    var deliveryFlags = flags
    if type == .mouseMoved || type == .leftMouseDragged || type == .rightMouseDragged {
        deliveryFlags.insert(.maskNonCoalesced)
    }
    event.flags = deliveryFlags
    event.timestamp = CGEventTimestamp(DispatchTime.now().uptimeNanoseconds)
    let clickCount = type == .mouseMoved ? 0 : 1
    let pressure: Double
    switch type {
    case .leftMouseDown, .leftMouseDragged, .rightMouseDown, .rightMouseDragged:
        pressure = 1
    default:
        pressure = 0
    }
    event.setIntegerValueField(.mouseEventNumber, value: eventNumber)
    event.setIntegerValueField(.mouseEventButtonNumber, value: Int64(button.rawValue))
    event.setIntegerValueField(.mouseEventClickState, value: Int64(clickCount))
    event.setDoubleValueField(.mouseEventPressure, value: pressure)
    return event
}

func pointerIsAt(_ point: CGPoint) -> Bool {
    guard let observed = CGEvent(source: nil)?.location else { return false }
    return abs(observed.x - point.x) <= 2 && abs(observed.y - point.y) <= 2
}

func positionPointer(at point: CGPoint) throws {
    let result = CGWarpMouseCursorPosition(point)
    guard result == .success else {
        throw DriverError.eventCreation("could not position the macOS pointer: \(result.rawValue)")
    }
    try waitForPointer(at: point)
}

func waitForPointer(at point: CGPoint) throws {
    let deadline = DispatchTime.now().uptimeNanoseconds + 50_000_000
    while !pointerIsAt(point) {
        guard DispatchTime.now().uptimeNanoseconds < deadline else {
            throw DriverError.unsafeGlobalDelivery("the system pointer did not reach the target point")
        }
        usleep(500)
    }
}

func postMouse(
    source: CGEventSource?,
    type: CGEventType,
    point: CGPoint,
    button: CGMouseButton,
    flags: CGEventFlags,
    pid: pid_t,
    handle: WindowHandle,
    eventNumber: Int64
) throws {
    try validateGlobalPointerRouting(
        pid: pid,
        windowId: handle.windowId,
        windowBounds: CGRect(
            x: handle.snapshot.globalBounds[0],
            y: handle.snapshot.globalBounds[1],
            width: handle.snapshot.globalBounds[2],
            height: handle.snapshot.globalBounds[3]
        ),
        contentBounds: CGRect(
            x: handle.snapshot.contentOrigin[0],
            y: handle.snapshot.contentOrigin[1],
            width: handle.snapshot.contentSize[0],
            height: handle.snapshot.contentSize[1]
        ),
        point: point
    )
    if type == .mouseMoved {
        try positionPointer(at: point)
    }
    let event = try makeGlobalMouseEvent(
        source: source,
        type: type,
        point: point,
        button: button,
        flags: flags,
        eventNumber: eventNumber
    )
    event.timestamp = CGEventTimestamp(DispatchTime.now().uptimeNanoseconds)
    event.post(tap: .cghidEventTap)
    try waitForPointer(at: point)
}

func postKeyboard(
    source: CGEventSource?, event: CompiledDriverEvent, down: Bool, pid: pid_t
) throws {
    guard let keyCode = event.keyCode,
          let native = CGEvent(keyboardEventSource: source, virtualKey: keyCode, keyDown: down) else {
        throw DriverError.eventCreation("could not create keyboard event")
    }
    native.flags = eventFlags(event.modifiers)
    if down, let text = event.text, !text.isEmpty {
        var utf16 = Array(text.utf16)
        native.keyboardSetUnicodeString(stringLength: utf16.count, unicodeString: &utf16)
    }
    native.postToPid(pid)
}

func postScroll(
    source: CGEventSource?,
    event: CompiledDriverEvent,
    point: CGPoint,
    pid: pid_t,
    handle: WindowHandle
) throws {
    let units: CGScrollEventUnit = event.scrollUnit == .pixel ? .pixel : .line
    guard let native = CGEvent(
        scrollWheelEvent2Source: source,
        units: units,
        wheelCount: 2,
        wheel1: Int32(event.deltaY.rounded()),
        wheel2: Int32(event.deltaX.rounded()),
        wheel3: 0
    ) else {
        throw DriverError.eventCreation("could not create scroll event")
    }
    native.flags = eventFlags(event.modifiers)
    native.setIntegerValueField(.scrollWheelEventIsContinuous, value: event.continuous ? 1 : 0)
    native.setIntegerValueField(.scrollWheelEventScrollPhase, value: phaseValue(event.phase))
    native.setIntegerValueField(
        .scrollWheelEventMomentumPhase,
        value: phaseValue(event.momentumPhase)
    )
    native.location = point
    try validateGlobalPointerRouting(
        pid: pid,
        windowId: handle.windowId,
        windowBounds: CGRect(
            x: handle.snapshot.globalBounds[0],
            y: handle.snapshot.globalBounds[1],
            width: handle.snapshot.globalBounds[2],
            height: handle.snapshot.globalBounds[3]
        ),
        contentBounds: CGRect(
            x: handle.snapshot.contentOrigin[0],
            y: handle.snapshot.contentOrigin[1],
            width: handle.snapshot.contentSize[0],
            height: handle.snapshot.contentSize[1]
        ),
        point: point
    )
    guard pointerIsAt(point) else {
        throw DriverError.unsafeGlobalDelivery("scroll target does not match the system pointer")
    }
    native.timestamp = CGEventTimestamp(DispatchTime.now().uptimeNanoseconds)
    native.post(tap: .cghidEventTap)
}

func setWindowSize(_ handle: WindowHandle, logicalSize: [UInt32]) throws -> [Double] {
    guard logicalSize.count == 2 else {
        throw DriverError.invalidRequest("resize event requires width and height")
    }
    var size = CGSize(
        width: Double(logicalSize[0]) + handle.chromeWidth,
        height: Double(logicalSize[1]) + handle.chromeHeight
    )
    guard let value = AXValueCreate(.cgSize, &size) else {
        throw DriverError.accessibility("could not create Accessibility size value")
    }
    let result = AXUIElementSetAttributeValue(handle.element, kAXSizeAttribute as CFString, value)
    guard result == .success else {
        throw DriverError.accessibility("could not resize RGV window: \(result.rawValue)")
    }
    return [Double(logicalSize[0]), Double(logicalSize[1])]
}

func emergencyRelease(
    pid: pid_t,
    source: CGEventSource?,
    point: CGPoint,
    keyCodes: [UInt16] = [],
    primaryEventNumber: Int64 = 1,
    secondaryEventNumber: Int64 = 2
) -> UInt64 {
    releaseHeldInput(
        pid: pid,
        source: source,
        point: point,
        primary: true,
        secondary: true,
        keyCodes: keyCodes + [55, 56, 58, 59, 60, 61, 62],
        primaryEventNumber: primaryEventNumber,
        secondaryEventNumber: secondaryEventNumber
    )
}

func releaseHeldInput(
    pid: pid_t,
    source: CGEventSource?,
    point: CGPoint,
    primary: Bool,
    secondary: Bool,
    keyCodes: [UInt16],
    primaryEventNumber: Int64 = 1,
    secondaryEventNumber: Int64 = 2
) -> UInt64 {
    var released: UInt64 = 0
    if primary,
       let event = try? makeGlobalMouseEvent(
           source: source,
           type: .leftMouseUp,
           point: point,
           button: .left,
           flags: [],
           eventNumber: primaryEventNumber
       ) {
        // A release must reach the global button state even if the routing guard failed after
        // its matching down event. This path can only emit mouse-up, never mouse-down.
        event.post(tap: .cghidEventTap)
        released += 1
    }
    if secondary,
       let event = try? makeGlobalMouseEvent(
           source: source,
           type: .rightMouseUp,
           point: point,
           button: .right,
           flags: [],
           eventNumber: secondaryEventNumber
       ) {
        event.post(tap: .cghidEventTap)
        released += 1
    }
    for keyCode in Set(keyCodes).sorted() {
        if let keyUp = CGEvent(
            keyboardEventSource: source, virtualKey: keyCode, keyDown: false
        ) {
            keyUp.postToPid(pid)
            released += 1
        }
    }
    return released
}

func waitUntil(_ deadline: UInt64) {
    while true {
        let now = DispatchTime.now().uptimeNanoseconds
        guard now < deadline else { return }
        let remaining = deadline - now
        if remaining > 200_000 {
            usleep(useconds_t(min(remaining / 1_000, 1_000)))
        }
    }
}

func focusApplicationWindow(
    application: NSRunningApplication,
    handle: WindowHandle,
    pid: pid_t
) throws {
    let applicationElement = AXUIElementCreateApplication(pid)
    let deadline = DispatchTime.now().uptimeNanoseconds + 2_000_000_000
    var frontmostResult = AXError.cannotComplete
    var raiseResult = AXError.cannotComplete
    var focusedResult = AXError.cannotComplete
    repeat {
        application.unhide()
        application.activate(options: [.activateAllWindows, .activateIgnoringOtherApps])
        frontmostResult = AXUIElementSetAttributeValue(
            applicationElement,
            kAXFrontmostAttribute as CFString,
            kCFBooleanTrue
        )
        raiseResult = AXUIElementPerformAction(handle.element, kAXRaiseAction as CFString)
        focusedResult = AXUIElementSetAttributeValue(
            handle.element,
            kAXFocusedAttribute as CFString,
            kCFBooleanTrue
        )
        if frontmostResult == .success,
           raiseResult == .success,
           focusedResult == .success,
           NSWorkspace.shared.frontmostApplication?.processIdentifier == pid {
            return
        }
        RunLoop.current.run(until: Date(timeIntervalSinceNow: 0.025))
    } while DispatchTime.now().uptimeNanoseconds < deadline

    for (operation, result) in [
        ("make RGV frontmost", frontmostResult),
        ("raise the selected RGV window", raiseResult),
        ("focus the selected RGV window", focusedResult),
    ] where result != .success {
        throw DriverError.accessibility("could not \(operation): \(result.rawValue)")
    }
    throw DriverError.windowNotFocused
}

@MainActor func execute(_ request: DriverRequest) throws -> DriverResponse {
    try request.validate()
    let permission = permissionState()
    guard permission == .granted else { throw DriverError.permissionDenied }
    let startingPointer = CGEvent(source: nil)?.location
    if request.operation == .restorePointer {
        guard let point = request.pointer else {
            throw DriverError.invalidRequest("restore_pointer omitted its point")
        }
        let target = CGPoint(x: point[0], y: point[1])
        try positionPointer(at: target)
        return DriverResponse(
            schema: driverSchema, schemaVersion: driverSchemaVersion,
            requestId: request.requestId, succeeded: true, permission: permission,
            failure: nil, window: nil, acknowledgements: [], emergencyReleases: 0,
            pointerPosition: [target.x, target.y]
        )
    }
    let beginsByRestoringFocus = request.events.first.map {
        $0.kind == .focus && $0.focused == true
    } ?? false
    let mayInspectHidden = beginsByRestoringFocus
        || request.operation == .preflight
        || request.operation == .emergencyRelease
    var handle = try inspectWindow(request: request, allowHidden: mayInspectHidden)
    guard let application = NSRunningApplication(processIdentifier: pid_t(request.processId)) else {
        throw DriverError.processMissing
    }
    if request.operation == .preflight {
        try focusApplicationWindow(
            application: application,
            handle: handle,
            pid: pid_t(request.processId)
        )
        handle = try inspectWindow(request: request)
        guard handle.snapshot.focused,
              NSWorkspace.shared.frontmostApplication?.processIdentifier
                  == pid_t(request.processId) else {
            throw DriverError.windowNotFocused
        }
        return DriverResponse(
            schema: driverSchema, schemaVersion: driverSchemaVersion,
            requestId: request.requestId, succeeded: true, permission: permission,
            failure: nil, window: handle.snapshot, acknowledgements: [], emergencyReleases: 0,
            pointerPosition: startingPointer.map { [$0.x, $0.y] }
        )
    }
    if request.operation == .inspectWindow {
        return DriverResponse(
            schema: driverSchema, schemaVersion: driverSchemaVersion,
            requestId: request.requestId, succeeded: true, permission: permission,
            failure: nil, window: handle.snapshot, acknowledgements: [], emergencyReleases: 0
        )
    }
    let pid = pid_t(request.processId)
    guard let source = CGEventSource(stateID: .hidSystemState) else {
        throw DriverError.eventCreation("could not create a HID-system event source")
    }
    var lastPoint = CGPoint(
        x: handle.snapshot.contentOrigin[0] + handle.snapshot.contentSize[0] / 2,
        y: handle.snapshot.contentOrigin[1] + handle.snapshot.contentSize[1] / 2
    )
    if request.operation == .emergencyRelease {
        let releases = emergencyRelease(
            pid: pid,
            source: source,
            point: lastPoint,
            keyCodes: request.events.compactMap(\.keyCode)
        )
        return DriverResponse(
            schema: driverSchema, schemaVersion: driverSchemaVersion,
            requestId: request.requestId, succeeded: true, permission: permission,
            failure: nil, window: handle.snapshot, acknowledgements: [], emergencyReleases: releases
        )
    }
    if !beginsByRestoringFocus && !handle.snapshot.focused {
        throw DriverError.windowNotFocused
    }
    let started = DispatchTime.now().uptimeNanoseconds
    var acknowledgements: [DriverAcknowledgement] = []
    var safetyReleases: UInt64 = 0
    var targetUnfocused = false
    var focusSentinel: FocusSentinel?
    defer { focusSentinel?.releaseFocus() }
    var primaryHeld = false
    var secondaryHeld = false
    var primaryMouseEventNumber: Int64?
    var secondaryMouseEventNumber: Int64?
    var nextMouseEventNumber = Int64(started % UInt64(Int32.max - 1)) + 1
    func freshMouseEventNumber() -> Int64 {
        let result = nextMouseEventNumber
        nextMouseEventNumber = result == Int64(Int32.max) ? 1 : result + 1
        return result
    }
    var keysHeld = Set<UInt16>()
    var previousRequestedMicros: UInt64 = 0
    var dispatchAnchor = started
    for event in request.events {
        let delayMicros = event.atMicros - previousRequestedMicros
        let delayNanos = delayMicros.multipliedReportingOverflow(by: 1_000)
        guard !delayNanos.overflow,
              dispatchAnchor <= UInt64.max - delayNanos.partialValue else {
            throw DriverError.invalidRequest("event timestamp overflowed")
        }
        waitUntil(dispatchAnchor + delayNanos.partialValue)
        if let point = event.point {
            lastPoint = CGPoint(
                x: handle.snapshot.contentOrigin[0] + point[0],
                y: handle.snapshot.contentOrigin[1] + point[1]
            )
        }
        let attempted = (DispatchTime.now().uptimeNanoseconds - started) / 1_000
        do {
            switch event.kind {
            case .move:
                let eventNumber = freshMouseEventNumber()
                try postMouse(
                    source: source,
                    type: .mouseMoved, point: lastPoint, button: .left,
                    flags: eventFlags(event.modifiers), pid: pid, handle: handle,
                    eventNumber: eventNumber
                )
            case .primaryDown:
                let eventNumber = freshMouseEventNumber()
                try postMouse(
                    source: source,
                    type: .leftMouseDown, point: lastPoint, button: .left,
                    flags: eventFlags(event.modifiers), pid: pid, handle: handle,
                    eventNumber: eventNumber
                )
                primaryMouseEventNumber = eventNumber
                primaryHeld = true
            case .primaryDrag:
                let eventNumber = primaryMouseEventNumber ?? freshMouseEventNumber()
                try postMouse(
                    source: source,
                    type: .leftMouseDragged, point: lastPoint, button: .left,
                    flags: eventFlags(event.modifiers), pid: pid, handle: handle,
                    eventNumber: eventNumber
                )
            case .primaryUp:
                let eventNumber = primaryMouseEventNumber ?? freshMouseEventNumber()
                try postMouse(
                    source: source,
                    type: .leftMouseUp, point: lastPoint, button: .left,
                    flags: eventFlags(event.modifiers), pid: pid, handle: handle,
                    eventNumber: eventNumber
                )
                primaryMouseEventNumber = nil
                primaryHeld = false
            case .secondaryDown:
                let eventNumber = freshMouseEventNumber()
                try postMouse(
                    source: source,
                    type: .rightMouseDown, point: lastPoint, button: .right,
                    flags: eventFlags(event.modifiers), pid: pid, handle: handle,
                    eventNumber: eventNumber
                )
                secondaryMouseEventNumber = eventNumber
                secondaryHeld = true
            case .secondaryDrag:
                let eventNumber = secondaryMouseEventNumber ?? freshMouseEventNumber()
                try postMouse(
                    source: source,
                    type: .rightMouseDragged, point: lastPoint, button: .right,
                    flags: eventFlags(event.modifiers), pid: pid, handle: handle,
                    eventNumber: eventNumber
                )
            case .secondaryUp:
                let eventNumber = secondaryMouseEventNumber ?? freshMouseEventNumber()
                try postMouse(
                    source: source,
                    type: .rightMouseUp, point: lastPoint, button: .right,
                    flags: eventFlags(event.modifiers), pid: pid, handle: handle,
                    eventNumber: eventNumber
                )
                secondaryMouseEventNumber = nil
                secondaryHeld = false
            case .scroll:
                try postScroll(
                    source: source, event: event, point: lastPoint, pid: pid,
                    handle: handle
                )
            case .keyDown:
                try postKeyboard(source: source, event: event, down: true, pid: pid)
                if let keyCode = event.keyCode { keysHeld.insert(keyCode) }
            case .keyUp:
                try postKeyboard(source: source, event: event, down: false, pid: pid)
                if let keyCode = event.keyCode { keysHeld.remove(keyCode) }
            case .focus:
                if event.focused == true {
                    focusSentinel?.yieldFocus(to: application)
                    focusSentinel = nil
                    try focusApplicationWindow(
                        application: application,
                        handle: handle,
                        pid: pid
                    )
                    targetUnfocused = false
                } else {
                    safetyReleases += releaseHeldInput(
                        pid: pid,
                        source: source,
                        point: lastPoint,
                        primary: primaryHeld,
                        secondary: secondaryHeld,
                        keyCodes: Array(keysHeld),
                        primaryEventNumber: primaryMouseEventNumber ?? freshMouseEventNumber(),
                        secondaryEventNumber: secondaryMouseEventNumber ?? freshMouseEventNumber()
                    )
                    primaryHeld = false
                    secondaryHeld = false
                    keysHeld.removeAll()
                    let sentinel = FocusSentinel()
                    try sentinel.takeFocus()
                    focusSentinel = sentinel
                    targetUnfocused = true
                }
            case .resizeWindow:
                guard let size = event.logicalSize else {
                    throw DriverError.invalidRequest("resize event omitted logical_size")
                }
                let contentSize = try setWindowSize(handle, logicalSize: size)
                handle = try inspectWindow(
                    request: request,
                    contentSize: contentSize,
                    enforceRequestBounds: false
                )
            }
            acknowledgements.append(
                DriverAcknowledgement(
                    sequence: event.sequence, requestedAtMicros: event.atMicros,
                    attemptedAtMicros: attempted, succeeded: true, failure: nil
                )
            )
            previousRequestedMicros = event.atMicros
            dispatchAnchor = DispatchTime.now().uptimeNanoseconds
        } catch {
            acknowledgements.append(
                DriverAcknowledgement(
                    sequence: event.sequence, requestedAtMicros: event.atMicros,
                    attemptedAtMicros: attempted, succeeded: false,
                    failure: String(describing: error)
                )
            )
            let keyCodes = request.events.compactMap(\.keyCode)
            let releases = emergencyRelease(
                pid: pid,
                source: source,
                point: lastPoint,
                keyCodes: keyCodes,
                primaryEventNumber: primaryMouseEventNumber ?? freshMouseEventNumber(),
                secondaryEventNumber: secondaryMouseEventNumber ?? freshMouseEventNumber()
            )
            return DriverResponse(
                schema: driverSchema, schemaVersion: driverSchemaVersion,
                requestId: request.requestId, succeeded: false, permission: permission,
                failure: String(describing: error), window: handle.snapshot,
                acknowledgements: acknowledgements, emergencyReleases: releases
            )
        }
    }
    do {
        if !targetUnfocused {
            handle = try inspectWindow(
                request: request,
                contentSize: handle.snapshot.contentSize,
                expectedBounds: handle.snapshot.globalBounds,
                enforceRequestBounds: false
            )
        }
    } catch {
        let releases = emergencyRelease(
            pid: pid,
            source: source,
            point: lastPoint,
            keyCodes: request.events.compactMap(\.keyCode)
        )
        return DriverResponse(
            schema: driverSchema, schemaVersion: driverSchemaVersion,
            requestId: request.requestId, succeeded: false, permission: permission,
            failure: String(describing: error), window: handle.snapshot,
            acknowledgements: acknowledgements, emergencyReleases: releases
        )
    }
    return DriverResponse(
        schema: driverSchema, schemaVersion: driverSchemaVersion,
        requestId: request.requestId, succeeded: true, permission: permission,
        failure: nil,
        window: targetUnfocused ? DriverWindow(
            processId: handle.snapshot.processId,
            processName: handle.snapshot.processName,
            title: handle.snapshot.title,
            globalBounds: handle.snapshot.globalBounds,
            contentOrigin: handle.snapshot.contentOrigin,
            contentSize: handle.snapshot.contentSize,
            focused: false
        ) : handle.snapshot,
        acknowledgements: acknowledgements,
        emergencyReleases: safetyReleases
    )
}

func writeResponse(_ response: DriverResponse, to url: URL) throws {
    let data = try encoder().encode(response)
    try data.write(to: url, options: .atomic)
}

func failureResponse(requestId: String, error: Error) -> DriverResponse {
    DriverResponse(
        schema: driverSchema, schemaVersion: driverSchemaVersion,
        requestId: requestId, succeeded: false, permission: permissionState(),
        failure: String(describing: error), window: nil, acknowledgements: [],
        emergencyReleases: 0
    )
}

@MainActor public func runDriverMain() -> Never {
  do {
    let arguments = try DriverArguments.parse(CommandLine.arguments)
    var requestId = "unknown"
    do {
        let data = try Data(contentsOf: arguments.request)
        if let identity = try? decoder().decode(DriverRequestIdentity.self, from: data) {
            requestId = identity.requestId
        }
        let request = try decoder().decode(DriverRequest.self, from: data)
        requestId = request.requestId
        let response = try execute(request)
        try writeResponse(response, to: arguments.response)
        exit(response.succeeded ? EXIT_SUCCESS : EXIT_FAILURE)
    } catch {
        try? writeResponse(failureResponse(requestId: requestId, error: error), to: arguments.response)
        fputs("rgv-macos-input-driver: \(error)\n", stderr)
        exit(EXIT_FAILURE)
    }
  } catch {
      fputs("rgv-macos-input-driver: \(error)\n", stderr)
      exit(EXIT_FAILURE)
  }
}
