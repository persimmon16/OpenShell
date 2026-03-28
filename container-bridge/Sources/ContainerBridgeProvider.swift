// SPDX-FileCopyrightText: Copyright (c) 2025-2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

import ContainerAPIClient
import GRPC
import Logging
import NIOCore
import SwiftProtobuf

/// gRPC service implementation that bridges to Apple Container's XPC API.
final class ContainerBridgeProvider: Openshell_Bridge_V1_ContainerBridgeAsyncProvider {
    let logger: Logger
    private let client: ContainerClient

    init(logger: Logger) {
        self.logger = logger
        self.client = ContainerClient()
    }

    // MARK: - Health

    func health(
        request: Openshell_Bridge_V1_BridgeHealthRequest,
        context: GRPCAsyncServerCallContext
    ) async throws -> Openshell_Bridge_V1_BridgeHealthResponse {
        var response = Openshell_Bridge_V1_BridgeHealthResponse()
        response.status = "ok"
        response.version = "0.1.0"
        return response
    }

    // MARK: - Container Lifecycle

    func createContainer(
        request: Openshell_Bridge_V1_CreateContainerRequest,
        context: GRPCAsyncServerCallContext
    ) async throws -> Openshell_Bridge_V1_ContainerResponse {
        logger.info("Creating container: \(request.name)")

        var config = ContainerConfiguration()
        config.name = request.name
        config.image = request.image

        for (key, value) in request.env {
            config.env[key] = value
        }

        if request.resources.cpus > 0 {
            config.cpus = Int(request.resources.cpus)
        }
        if request.resources.memoryMb > 0 {
            config.memoryMB = Int(request.resources.memoryMb)
        }

        let container = try await client.create(config)
        return containerToProto(container)
    }

    func startContainer(
        request: Openshell_Bridge_V1_StartContainerRequest,
        context: GRPCAsyncServerCallContext
    ) async throws -> Openshell_Bridge_V1_StartContainerResponse {
        logger.info("Starting container: \(request.name)")
        try await client.start(request.name)
        return Openshell_Bridge_V1_StartContainerResponse()
    }

    func stopContainer(
        request: Openshell_Bridge_V1_StopContainerRequest,
        context: GRPCAsyncServerCallContext
    ) async throws -> Openshell_Bridge_V1_StopContainerResponse {
        logger.info("Stopping container: \(request.name)")
        let timeout = request.timeoutSeconds > 0 ? Int(request.timeoutSeconds) : 10
        try await client.stop(request.name, timeout: timeout)
        return Openshell_Bridge_V1_StopContainerResponse()
    }

    func deleteContainer(
        request: Openshell_Bridge_V1_DeleteContainerRequest,
        context: GRPCAsyncServerCallContext
    ) async throws -> Openshell_Bridge_V1_DeleteContainerResponse {
        logger.info("Deleting container: \(request.name)")
        try await client.delete(request.name, force: request.force)
        return Openshell_Bridge_V1_DeleteContainerResponse()
    }

    func getContainer(
        request: Openshell_Bridge_V1_GetContainerRequest,
        context: GRPCAsyncServerCallContext
    ) async throws -> Openshell_Bridge_V1_ContainerResponse {
        let container = try await client.get(request.name)
        return containerToProto(container)
    }

    func listContainers(
        request: Openshell_Bridge_V1_ListContainersRequest,
        context: GRPCAsyncServerCallContext
    ) async throws -> Openshell_Bridge_V1_ListContainersResponse {
        let containers = try await client.list(all: request.all)
        var response = Openshell_Bridge_V1_ListContainersResponse()
        response.containers = containers.map { containerToProto($0) }
        return response
    }

    // MARK: - Watch

    func watchContainers(
        request: Openshell_Bridge_V1_WatchContainersRequest,
        responseStream: GRPCAsyncResponseStreamWriter<Openshell_Bridge_V1_ContainerEvent>,
        context: GRPCAsyncServerCallContext
    ) async throws {
        var previousState: [String: Openshell_Bridge_V1_ContainerState] = [:]

        while !Task.isCancelled {
            let containers = try await client.list(all: true)

            var currentState: [String: Openshell_Bridge_V1_ContainerState] = [:]
            for container in containers {
                let proto = containerToProto(container)
                currentState[proto.name] = proto.state

                if let previous = previousState[proto.name] {
                    if previous != proto.state {
                        var event = Openshell_Bridge_V1_ContainerEvent()
                        event.container = proto
                        event.type = proto.state == .running ? .started : .stopped
                        try await responseStream.send(event)
                    }
                } else {
                    var event = Openshell_Bridge_V1_ContainerEvent()
                    event.container = proto
                    event.type = .created
                    try await responseStream.send(event)
                }
            }

            for (name, _) in previousState {
                if currentState[name] == nil {
                    var event = Openshell_Bridge_V1_ContainerEvent()
                    event.container.name = name
                    event.type = .deleted
                    try await responseStream.send(event)
                }
            }

            previousState = currentState
            try await Task.sleep(for: .seconds(2))
        }
    }

    // MARK: - Logs

    func containerLogs(
        request: Openshell_Bridge_V1_ContainerLogsRequest,
        responseStream: GRPCAsyncResponseStreamWriter<Openshell_Bridge_V1_LogChunk>,
        context: GRPCAsyncServerCallContext
    ) async throws {
        let logs = try await client.logs(
            request.name,
            follow: request.follow,
            tail: Int(request.tailLines)
        )

        for try await line in logs {
            var chunk = Openshell_Bridge_V1_LogChunk()
            chunk.data = Data(line.utf8)
            chunk.stream = .stdout
            try await responseStream.send(chunk)
        }
    }

    // MARK: - Exec

    func execContainer(
        request: Openshell_Bridge_V1_ExecContainerRequest,
        responseStream: GRPCAsyncResponseStreamWriter<Openshell_Bridge_V1_ExecEvent>,
        context: GRPCAsyncServerCallContext
    ) async throws {
        let result = try await client.exec(
            request.name,
            command: request.command
        )

        if !result.stdout.isEmpty {
            var event = Openshell_Bridge_V1_ExecEvent()
            event.stdout = Data(result.stdout.utf8)
            try await responseStream.send(event)
        }

        if !result.stderr.isEmpty {
            var event = Openshell_Bridge_V1_ExecEvent()
            event.stderr = Data(result.stderr.utf8)
            try await responseStream.send(event)
        }

        var exitEvent = Openshell_Bridge_V1_ExecEvent()
        exitEvent.exitCode = result.exitCode
        try await responseStream.send(exitEvent)
    }

    // MARK: - Images

    func pullImage(
        request: Openshell_Bridge_V1_PullImageRequest,
        responseStream: GRPCAsyncResponseStreamWriter<Openshell_Bridge_V1_PullProgress>,
        context: GRPCAsyncServerCallContext
    ) async throws {
        logger.info("Pulling image: \(request.reference)")

        let progress = try await client.pullImage(request.reference)
        for try await update in progress {
            var msg = Openshell_Bridge_V1_PullProgress()
            msg.status = update.status
            msg.detail = update.detail ?? ""
            msg.progressPercent = update.progress ?? 0
            try await responseStream.send(msg)
        }
    }

    func listImages(
        request: Openshell_Bridge_V1_ListImagesRequest,
        context: GRPCAsyncServerCallContext
    ) async throws -> Openshell_Bridge_V1_ListImagesResponse {
        let images = try await client.listImages()
        var response = Openshell_Bridge_V1_ListImagesResponse()
        response.images = images.map { img in
            var info = Openshell_Bridge_V1_ImageInfo()
            info.id = img.id
            info.tags = img.tags
            info.sizeBytes = UInt64(img.size)
            return info
        }
        return response
    }

    func deleteImage(
        request: Openshell_Bridge_V1_DeleteImageRequest,
        context: GRPCAsyncServerCallContext
    ) async throws -> Openshell_Bridge_V1_DeleteImageResponse {
        try await client.deleteImage(request.reference, force: request.force)
        return Openshell_Bridge_V1_DeleteImageResponse()
    }

    // MARK: - Network

    func createNetwork(
        request: Openshell_Bridge_V1_CreateNetworkRequest,
        context: GRPCAsyncServerCallContext
    ) async throws -> Openshell_Bridge_V1_NetworkResponse {
        try await client.createNetwork(request.name)
        var response = Openshell_Bridge_V1_NetworkResponse()
        response.name = request.name
        return response
    }

    func deleteNetwork(
        request: Openshell_Bridge_V1_DeleteNetworkRequest,
        context: GRPCAsyncServerCallContext
    ) async throws -> Openshell_Bridge_V1_DeleteNetworkResponse {
        try await client.deleteNetwork(request.name)
        return Openshell_Bridge_V1_DeleteNetworkResponse()
    }

    // MARK: - Helpers

    private func containerToProto(_ container: ContainerInfo) -> Openshell_Bridge_V1_ContainerResponse {
        var proto = Openshell_Bridge_V1_ContainerResponse()
        proto.id = container.id
        proto.name = container.name
        proto.image = container.image
        proto.state = stateToProto(container.state)
        proto.labels = container.labels
        return proto
    }

    private func stateToProto(_ state: String) -> Openshell_Bridge_V1_ContainerState {
        switch state.lowercased() {
        case "running": return .running
        case "created": return .created
        case "stopped": return .stopped
        case "exited": return .exited
        default: return .unknown
        }
    }
}
