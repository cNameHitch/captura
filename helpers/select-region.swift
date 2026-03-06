#!/usr/bin/env swift
// Displays a full-screen transparent overlay and lets the user drag to select a region.
// Prints "x,y,width,height" to stdout (in screen points), or exits with code 1 if cancelled.

import Cocoa

class RegionSelectionWindow: NSWindow {
    var startPoint: NSPoint?
    var currentPoint: NSPoint?

    convenience init(screen: NSScreen) {
        self.init(
            contentRect: screen.frame,
            styleMask: .borderless,
            backing: .buffered,
            defer: false
        )
        self.setFrame(screen.frame, display: true)
        self.level = .statusBar + 1
        self.isOpaque = false
        self.backgroundColor = NSColor.black.withAlphaComponent(0.01)
        self.ignoresMouseEvents = false
        self.acceptsMouseMovedEvents = true
        self.hasShadow = false

        let selView = SelectionOverlay(frame: screen.frame)
        selView.regionWindow = self
        self.contentView = selView

        NSCursor.crosshair.push()
    }

    override var canBecomeKey: Bool { true }
    override var canBecomeMain: Bool { true }

    override func mouseDown(with event: NSEvent) {
        startPoint = NSEvent.mouseLocation
        currentPoint = startPoint
        contentView?.needsDisplay = true
    }

    override func mouseDragged(with event: NSEvent) {
        currentPoint = NSEvent.mouseLocation
        contentView?.needsDisplay = true
    }

    override func mouseUp(with event: NSEvent) {
        currentPoint = NSEvent.mouseLocation
        guard let start = startPoint, let end = currentPoint else {
            NSCursor.pop()
            exit(1)
        }

        let x = Int(min(start.x, end.x))
        let w = Int(abs(end.x - start.x))

        // Convert from bottom-left (Cocoa) to top-left (screen) coordinates
        guard let screenFrame = self.screen?.frame else { exit(1) }
        let screenHeight = Int(screenFrame.height)
        let topY = Int(max(start.y, end.y))
        let y = screenHeight - topY
        let h = Int(abs(end.y - start.y))

        if w > 5 && h > 5 {
            print("\(x),\(y),\(w),\(h)")
            NSCursor.pop()
            exit(0)
        } else {
            NSCursor.pop()
            exit(1)
        }
    }

    override func keyDown(with event: NSEvent) {
        if event.keyCode == 53 { // Escape
            NSCursor.pop()
            exit(1)
        }
    }
}

class SelectionOverlay: NSView {
    var regionWindow: RegionSelectionWindow?

    override func draw(_ dirtyRect: NSRect) {
        super.draw(dirtyRect)

        // Semi-transparent overlay
        NSColor.black.withAlphaComponent(0.25).setFill()
        bounds.fill()

        guard let rw = regionWindow,
              let start = rw.startPoint,
              let current = rw.currentPoint,
              let nsWin = self.window else {
            return
        }

        // Convert screen coords to view coords
        let startInWindow = nsWin.convertPoint(fromScreen: start)
        let currentInWindow = nsWin.convertPoint(fromScreen: current)
        let startLocal = self.convert(startInWindow, from: nil)
        let currentLocal = self.convert(currentInWindow, from: nil)

        let selRect = NSRect(
            x: min(startLocal.x, currentLocal.x),
            y: min(startLocal.y, currentLocal.y),
            width: abs(currentLocal.x - startLocal.x),
            height: abs(currentLocal.y - startLocal.y)
        )

        // Clear the selection area
        NSColor.clear.setFill()
        selRect.fill(using: .sourceOver)
        NSGraphicsContext.current?.cgContext.clear(selRect)

        // Draw border
        NSColor.white.setStroke()
        let path = NSBezierPath(rect: selRect)
        path.lineWidth = 2
        path.stroke()
    }
}

class AppDelegate: NSObject, NSApplicationDelegate {}

let app = NSApplication.shared
app.setActivationPolicy(.accessory)
let delegate = AppDelegate()
app.delegate = delegate

guard let screen = NSScreen.main else {
    fputs("No screen found\n", stderr)
    exit(1)
}

let window = RegionSelectionWindow(screen: screen)
window.makeKeyAndOrderFront(nil)
app.activate(ignoringOtherApps: true)

app.run()
