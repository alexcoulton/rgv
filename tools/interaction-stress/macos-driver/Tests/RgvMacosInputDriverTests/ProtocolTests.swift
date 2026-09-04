import AppKit
import Foundation
import Testing
@testable import RgvMacosInputDriverCore

@Suite struct ProtocolTests {
    @Test func snakeCaseWireEnumsMatchRustSerde() throws {
        let jsonDecoder = JSONDecoder()
        for (wireValue, expected) in [
            ("preflight", DriverOperation.preflight),
            ("inspect_window", .inspectWindow),
            ("deliver", .deliver),
            ("emergency_release", .emergencyRelease),
            ("restore_pointer", .restorePointer),
        ] {
            let observed = try jsonDecoder.decode(
                DriverOperation.self,
                from: Data("\"\(wireValue)\"".utf8)
            )
            #expect(observed == expected)
        }

        for (wireValue, expected) in [
            ("move", DriverEventKind.move),
            ("primary_down", .primaryDown),
            ("primary_drag", .primaryDrag),
            ("primary_up", .primaryUp),
            ("secondary_down", .secondaryDown),
            ("secondary_drag", .secondaryDrag),
            ("secondary_up", .secondaryUp),
            ("scroll", .scroll),
            ("key_down", .keyDown),
            ("key_up", .keyUp),
            ("focus", .focus),
            ("resize_window", .resizeWindow),
        ] {
            let observed = try jsonDecoder.decode(
                DriverEventKind.self,
                from: Data("\"\(wireValue)\"".utf8)
            )
            #expect(observed == expected)
        }

        let mayBegin = try jsonDecoder.decode(TouchPhase.self, from: Data(#""may_begin""#.utf8))
        #expect(mayBegin == .mayBegin)
    }

    @Test func malformedRequestRetainsItsCorrelationIdentity() throws {
        let json = #"{"request_id":"request-from-rust","events":[{"kind":"not_real"}]}"#
        let data = Data(json.utf8)
        let identity = try decoder().decode(DriverRequestIdentity.self, from: data)
        #expect(identity.requestId == "request-from-rust")
        #expect(throws: DecodingError.self) {
            try decoder().decode(DriverRequest.self, from: data)
        }
    }

    @Test func windowServerIdentityUsesPidBoundsAndTitle() throws {
        let bounds = CGRect(x: 100, y: 50, width: 1280, height: 828)
        let windows = [
            WindowServerWindow(
                id: 9, processId: 42, layer: 0, bounds: bounds, title: "Another window"
            ),
            WindowServerWindow(
                id: 11, processId: 42, layer: 0, bounds: bounds,
                title: "RGV — Rust Genome Viewer"
            ),
            WindowServerWindow(
                id: 13, processId: 43, layer: 0, bounds: bounds,
                title: "RGV — Rust Genome Viewer"
            ),
        ]
        let selected = try selectWindowServerWindowId(
            windows: windows,
            processId: 42,
            expectedBounds: bounds,
            expectedTitle: "RGV — Rust Genome Viewer"
        )
        #expect(selected == 11)
    }

    @Test func mouseEventCarriesNativeIdentityAndGlobalCoordinates() throws {
        let point = CGPoint(x: 455.5, y: 578.1)
        let event = try makeGlobalMouseEvent(
            source: CGEventSource(stateID: .hidSystemState),
            type: .leftMouseDragged,
            point: point,
            button: .left,
            flags: [.maskShift],
            eventNumber: 912
        )
        #expect(event.type == .leftMouseDragged)
        #expect(event.location == point)
        #expect(event.flags.contains(.maskShift))
        #expect(event.flags.contains(.maskNonCoalesced))
        #expect(event.getIntegerValueField(.mouseEventClickState) == 1)
        #expect(event.getIntegerValueField(.mouseEventNumber) == 912)
        #expect(event.getIntegerValueField(.mouseEventButtonNumber) == 0)
        #expect(event.getDoubleValueField(.mouseEventPressure) == 1)
    }

    @Test func globalPointerRoutingRequiresTheExactFrontmostWindow() throws {
        let windowBounds = CGRect(x: 100, y: 50, width: 1280, height: 828)
        let contentBounds = CGRect(x: 100, y: 78, width: 1280, height: 800)
        let point = CGPoint(x: 455.5, y: 578.1)
        let expected = WindowServerWindow(
            id: 77,
            processId: 42,
            layer: 0,
            bounds: windowBounds,
            title: "RGV — Rust Genome Viewer"
        )
        try validateGlobalPointerRouting(
            expectedProcessId: 42,
            expectedWindowId: 77,
            expectedWindowBounds: windowBounds,
            expectedContentBounds: contentBounds,
            point: point,
            frontmostProcessId: 42,
            windows: [expected]
        )

        let coveringWindow = WindowServerWindow(
            id: 88,
            processId: 99,
            layer: 0,
            bounds: CGRect(x: 400, y: 500, width: 200, height: 200),
            title: "Unexpected overlay"
        )
        #expect(throws: DriverError.self) {
            try validateGlobalPointerRouting(
                expectedProcessId: 42,
                expectedWindowId: 77,
                expectedWindowBounds: windowBounds,
                expectedContentBounds: contentBounds,
                point: point,
                frontmostProcessId: 42,
                windows: [coveringWindow, expected]
            )
        }
        #expect(throws: DriverError.self) {
            try validateGlobalPointerRouting(
                expectedProcessId: 42,
                expectedWindowId: 77,
                expectedWindowBounds: windowBounds,
                expectedContentBounds: contentBounds,
                point: point,
                frontmostProcessId: 99,
                windows: [expected]
            )
        }
    }

    @Test func boundedRequestDecodesAndValidates() throws {
        let json = #"""
        {
          "schema":"rgv.macos_input_driver",
          "schema_version":1,
          "request_id":"request-1",
          "operation":"deliver",
          "application_instance_id":"instance-1",
          "run_id":"run-1",
          "step_id":"step-1",
          "process_id":42,
          "expected_process_name":"rgv-desktop",
          "expected_window_title_prefix":"RGV",
          "expected_content_size":[1280.0,800.0],
          "maximum_duration_millis":1000,
          "events":[{
            "sequence":1,"at_micros":0,"kind":"move","point":[20.0,30.0],
            "delta_x":0.0,"delta_y":0.0,"scroll_unit":"pixel","phase":"none",
            "momentum_phase":"none","continuous":false,
            "modifiers":{"shift":false,"control":false,"option":false,"command":false}
          }]
        }
        """#.data(using: .utf8)!
        let decoder = JSONDecoder()
        decoder.keyDecodingStrategy = .convertFromSnakeCase
        let request = try decoder.decode(DriverRequest.self, from: json)
        try request.validate()
        #expect(request.events.count == 1)
        #expect(request.events[0].point == [20.0, 30.0])
        #expect(request.expectedWindowBounds == nil)
    }

    @Test func malformedExpectedWindowBoundsAreRejected() throws {
        var request = DriverRequest(
            schema: driverSchema, schemaVersion: driverSchemaVersion, requestId: "bounds",
            operation: .preflight, applicationInstanceId: "instance-1", runId: "preflight",
            stepId: "preflight", processId: 1, expectedProcessName: "rgv-desktop",
            expectedWindowTitlePrefix: "RGV", expectedContentSize: [100, 100],
            maximumDurationMillis: 100, events: []
        )
        request.expectedWindowBounds = [0, 0, -1, 100]
        #expect(throws: DriverError.self) {
            try request.validate()
        }
    }

    @Test func staleContentDimensionsCannotProduceAFalseContentOrigin() throws {
        let valid = try validatedChromeDimensions(
            windowSize: CGSize(width: 1180, height: 768),
            contentSize: [1180, 740]
        )
        #expect(valid.width == 0)
        #expect(valid.height == 28)

        #expect(throws: DriverError.self) {
            try validatedChromeDimensions(
                windowSize: CGSize(width: 1180, height: 768),
                contentSize: [1280, 800]
            )
        }
    }

    @Test func nonMonotonicScheduleIsRejected() throws {
        let event = CompiledDriverEvent(
            sequence: 1, atMicros: 10, kind: .move, point: [1, 1], deltaX: 0,
            deltaY: 0, scrollUnit: .pixel, phase: .none, momentumPhase: .none,
            continuous: false, keyCode: nil, text: nil, focused: nil, logicalSize: nil,
            modifiers: StressModifiers()
        )
        let request = DriverRequest(
            schema: "rgv.macos_input_driver", schemaVersion: 1, requestId: "r",
            operation: .deliver, applicationInstanceId: "instance-1", runId: "run-1",
            stepId: "step-1", processId: 1, expectedProcessName: "rgv-desktop",
            expectedWindowTitlePrefix: "RGV", expectedContentSize: [100, 100],
            maximumDurationMillis: 100, events: [event, CompiledDriverEvent(
                sequence: 2, atMicros: 9, kind: .move, point: [1, 1], deltaX: 0,
                deltaY: 0, scrollUnit: .pixel, phase: .none, momentumPhase: .none,
                continuous: false, keyCode: nil, text: nil, focused: nil, logicalSize: nil,
                modifiers: StressModifiers()
            )]
        )
        #expect(throws: DriverError.self) {
            try request.validate()
        }
    }

    @Test func emergencyReleaseAcceptsOnlyKeyUps() throws {
        let keyUp = CompiledDriverEvent(
            sequence: 1, atMicros: 0, kind: .keyUp, point: nil, deltaX: 0,
            deltaY: 0, scrollUnit: .pixel, phase: .none, momentumPhase: .none,
            continuous: false, keyCode: 12, text: nil, focused: nil, logicalSize: nil,
            modifiers: StressModifiers()
        )
        var request = DriverRequest(
            schema: driverSchema, schemaVersion: driverSchemaVersion, requestId: "release",
            operation: .emergencyRelease, applicationInstanceId: "instance-1", runId: "run-1",
            stepId: "step-1", processId: 1, expectedProcessName: "rgv-desktop",
            expectedWindowTitlePrefix: "RGV", expectedContentSize: [100, 100],
            maximumDurationMillis: 100, events: [keyUp]
        )
        try request.validate()
        request = DriverRequest(
            schema: driverSchema, schemaVersion: driverSchemaVersion, requestId: "release",
            operation: .emergencyRelease, applicationInstanceId: "instance-1", runId: "run-1",
            stepId: "step-1", processId: 1, expectedProcessName: "rgv-desktop",
            expectedWindowTitlePrefix: "RGV", expectedContentSize: [100, 100],
            maximumDurationMillis: 100,
            events: [CompiledDriverEvent(
                sequence: 1, atMicros: 0, kind: .keyDown, point: nil, deltaX: 0,
                deltaY: 0, scrollUnit: .pixel, phase: .none, momentumPhase: .none,
                continuous: false, keyCode: 12, text: nil, focused: nil, logicalSize: nil,
                modifiers: StressModifiers()
            )]
        )
        #expect(throws: DriverError.self) {
            try request.validate()
        }
    }
}
