---
name: integration-grpc
version: "1.0"
description: gRPC service design with Protocol Buffers — proto definitions, code generation, streaming, and error handling
categories: [api, integration, backend]
triggers: ["grpc service protobuf", "proto file definition", "grpc code generate", "grpc streaming server", "grpc error handling deadline"]
tools: [read_file, edit_file, shell, run_tests]
---

# gRPC Service Design Skill

## Proto Definition Best Practices
```proto
syntax = "proto3";
package orders.v1;
option go_package = "github.com/org/app/gen/orders/v1;ordersv1";

import "google/protobuf/timestamp.proto";
import "google/rpc/status.proto";

service OrderService {
  rpc CreateOrder(CreateOrderRequest) returns (Order);
  rpc GetOrder(GetOrderRequest) returns (Order);
  rpc ListOrders(ListOrdersRequest) returns (stream Order);     // server streaming
}

message Order {
  string id = 1;
  string customer_id = 2;
  repeated OrderItem items = 3;
  google.protobuf.Timestamp created_at = 4;
}

message CreateOrderRequest {
  string customer_id = 1;
  repeated OrderItem items = 2;
}

message GetOrderRequest {
  string id = 1;
}
```

## Code Generation
```bash
# Python
pip install grpcio grpcio-tools
python -m grpc_tools.protoc -I. \
  --python_out=. --grpc_python_out=. \
  orders/v1/orders.proto

# TypeScript (ts-proto)
npm install -D ts-proto
protoc --plugin=./node_modules/.bin/protoc-gen-ts_proto \
  --ts_proto_out=src/gen --ts_proto_opt=env=node orders.proto
```

## Python Async Server
```python
import grpc
import orders_pb2, orders_pb2_grpc

class OrderServiceServicer(orders_pb2_grpc.OrderServiceServicer):
    async def CreateOrder(self, request, context):
        order = await db.create_order(request.customer_id, list(request.items))
        return orders_pb2.Order(id=str(order.id), customer_id=order.customer_id)

    async def ListOrders(self, request, context):
        async for order in db.stream_orders(request.customer_id):
            yield orders_pb2.Order(id=str(order.id))

    async def GetOrder(self, request, context):
        order = await db.get_order(request.id)
        if not order:
            context.set_code(grpc.StatusCode.NOT_FOUND)
            context.set_details(f"Order {request.id} not found")
            return orders_pb2.Order()
        return orders_pb2.Order(id=order.id, customer_id=order.customer_id)

server = grpc.aio.server()
orders_pb2_grpc.add_OrderServiceServicer_to_server(OrderServiceServicer(), server)
server.add_insecure_port("[::]:50051")
await server.start()
await server.wait_for_termination()
```

## Error Handling
```python
context.set_code(grpc.StatusCode.NOT_FOUND)
context.set_details("Order not found")
return orders_pb2.Order()   # return empty message with status code
```
Map domain errors to gRPC status codes: `INVALID_ARGUMENT`, `NOT_FOUND`, `ALREADY_EXISTS`, `PERMISSION_DENIED`.

## Versioning and Compatibility
- Version all packages: `orders.v1`, `orders.v2` — never break the same version
- Adding fields is safe; removing or renaming fields is a breaking change
- Use field numbers 1–15 for frequently-used fields (single-byte encoding)

## Production Checklist
- Always set deadlines; clients should pass deadline/timeout on every call
- Implement `grpc.health.v1` health check service for load balancer probes
- Enable mTLS for service-to-service communication in production
- Use `google.rpc.Status` with `details` for structured rich error responses
