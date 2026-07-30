#pragma once

// The probe resolves CUDA Runtime functions dynamically and only needs the
// opaque stream/event ABI types required by TensorRT-RTX's public headers.
struct CUstream_st;
struct CUevent_st;
using cudaStream_t = CUstream_st*;
using cudaEvent_t = CUevent_st*;
