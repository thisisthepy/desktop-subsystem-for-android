#!/bin/sh
# Stands in for the protobuf compiler, which no part of the translator build runs.
echo "aprotoc is not in this reduced tree; a module that compiles .proto files needs external/protobuf" >&2
exit 1
