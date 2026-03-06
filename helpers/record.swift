#!/usr/bin/env swift
// Native macOS screen recorder using ScreenCaptureKit + AVAssetWriter.
// Usage: swift record.swift <output.mov> [display_index]
// Send SIGINT (Ctrl+C) to stop recording gracefully.

import Foundation
import ScreenCaptureKit
import AVFoundation
import CoreMedia

guard CommandLine.arguments.count >= 2 else {
    fputs("Usage: record.swift <output.mov> [display_index]\n", stderr)
    exit(1)
}

let outputPath = CommandLine.arguments[1]
let displayIndex = CommandLine.arguments.count > 2 ? Int(CommandLine.arguments[2]) ?? 0 : 0
let outputURL = URL(fileURLWithPath: outputPath)

// Delete existing file
try? FileManager.default.removeItem(at: outputURL)

class Recorder: NSObject, SCStreamOutput {
    var stream: SCStream?
    var writer: AVAssetWriter?
    var videoInput: AVAssetWriterInput?
    var started = false
    var sessionStarted = false

    func start() async throws {
        let content = try await SCShareableContent.current
        guard displayIndex < content.displays.count else {
            fputs("Display index \(displayIndex) not found. Available: \(content.displays.count)\n", stderr)
            exit(1)
        }

        let display = content.displays[displayIndex]
        let filter = SCContentFilter(display: display, excludingWindows: [])
        let config = SCStreamConfiguration()
        config.width = display.width * 2  // Retina
        config.height = display.height * 2
        config.minimumFrameInterval = CMTime(value: 1, timescale: 30)
        config.showsCursor = true
        config.pixelFormat = kCVPixelFormatType_32BGRA

        writer = try AVAssetWriter(outputURL: outputURL, fileType: .mov)

        let videoSettings: [String: Any] = [
            AVVideoCodecKey: AVVideoCodecType.h264,
            AVVideoWidthKey: config.width,
            AVVideoHeightKey: config.height,
        ]
        videoInput = AVAssetWriterInput(mediaType: .video, outputSettings: videoSettings)
        videoInput!.expectsMediaDataInRealTime = true
        writer!.add(videoInput!)
        writer!.startWriting()

        stream = SCStream(filter: filter, configuration: config, delegate: nil)
        try stream!.addStreamOutput(self, type: .screen, sampleHandlerQueue: .global())
        try await stream!.startCapture()

        started = true
        fputs("Recording started: \(outputPath)\n", stderr)
    }

    func stop() async {
        guard started else { return }
        started = false

        try? await stream?.stopCapture()

        videoInput?.markAsFinished()
        await writer?.finishWriting()

        fputs("Recording saved: \(outputPath)\n", stderr)
    }

    func stream(_ stream: SCStream, didOutputSampleBuffer sampleBuffer: CMSampleBuffer, of type: SCStreamOutputType) {
        guard type == .screen, started, let videoInput = videoInput, videoInput.isReadyForMoreMediaData else {
            return
        }

        if !sessionStarted {
            let timestamp = CMSampleBufferGetPresentationTimeStamp(sampleBuffer)
            writer?.startSession(atSourceTime: timestamp)
            sessionStarted = true
        }

        videoInput.append(sampleBuffer)
    }
}

let recorder = Recorder()

// Handle SIGINT for graceful stop
let sigintSource = DispatchSource.makeSignalSource(signal: SIGINT, queue: .main)
signal(SIGINT, SIG_IGN)
sigintSource.setEventHandler {
    Task {
        await recorder.stop()
        exit(0)
    }
}
sigintSource.resume()

// Handle SIGTERM too
let sigtermSource = DispatchSource.makeSignalSource(signal: SIGTERM, queue: .main)
signal(SIGTERM, SIG_IGN)
sigtermSource.setEventHandler {
    Task {
        await recorder.stop()
        exit(0)
    }
}
sigtermSource.resume()

// Start recording
Task {
    do {
        try await recorder.start()
    } catch {
        fputs("Error: \(error)\n", stderr)
        exit(1)
    }
}

RunLoop.main.run()
