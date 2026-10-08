// SPDX-License-Identifier: MIT
// Project-owned RIFE builder; NVIDIA SDK binaries retain their own licenses.
#include <NvInfer.h>
#include <NvOnnxParser.h>

#include <array>
#include <cstdio>
#include <filesystem>
#include <fstream>
#include <memory>
#include <stdexcept>
#include <string>
#include <string_view>
#include <vector>

namespace {

struct Profile {
    std::array<nvinfer1::Dims4, 3> shapes{};
    std::array<bool, 3> supplied{};
};

struct Arguments {
    std::filesystem::path onnx;
    std::filesystem::path engine;
    std::vector<Profile> profiles;
};

int parse_integer(std::wstring_view text, int maximum)
{
    if (text.empty())
        throw std::runtime_error("Empty integer argument");
    int value = 0;
    for (const wchar_t digit : text) {
        if (digit < L'0' || digit > L'9')
            throw std::runtime_error("Integer arguments must contain only digits");
        const int next = static_cast<int>(digit - L'0');
        if (value > (maximum - next) / 10 || next > maximum)
            throw std::runtime_error("Integer argument exceeds its supported limit");
        value = value * 10 + next;
    }
    return value;
}

nvinfer1::Dims4 parse_shape(std::wstring_view text)
{
    constexpr std::wstring_view prefix = L"input:1x11x";
    if (!text.starts_with(prefix))
        throw std::runtime_error("RIFE shapes must be input:1x11xHEIGHTxWIDTH");
    text.remove_prefix(prefix.size());
    const size_t separator = text.find(L'x');
    if (separator == std::wstring_view::npos)
        throw std::runtime_error("RIFE shape is missing its width");
    const int height = parse_integer(text.substr(0, separator), 16384);
    const int width = parse_integer(text.substr(separator + 1), 16384);
    if (height < 64 || width < 64 || height % 64 || width % 64)
        throw std::runtime_error("RIFE dimensions must be positive multiples of 64");
    return {1, 11, height, width};
}

Arguments parse_arguments(int argc, wchar_t** argv)
{
    Arguments result;
    for (int index = 1; index < argc; ++index) {
        const std::wstring_view argument(argv[index]);
        const size_t separator = argument.find(L'=');
        if (separator == std::wstring_view::npos)
            throw std::runtime_error("Every argument must use --name=value");
        const auto name = argument.substr(0, separator);
        const auto value = argument.substr(separator + 1);
        if (name == L"--onnx" || name == L"--saveEngine") {
            auto& path = name == L"--onnx" ? result.onnx : result.engine;
            if (!path.empty() || value.empty())
                throw std::runtime_error("Missing or duplicate file path argument");
            path = std::filesystem::path(value);
        } else if (name == L"--profile") {
            if (parse_integer(value, 3) != static_cast<int>(result.profiles.size()))
                throw std::runtime_error("Profiles must be ordered consecutively from zero");
            result.profiles.emplace_back();
        } else {
            size_t selector = 0;
            if (name == L"--optShapes")
                selector = 1;
            else if (name == L"--maxShapes")
                selector = 2;
            else if (name != L"--minShapes")
                throw std::runtime_error("Unsupported RIFE builder argument");
            if (result.profiles.empty())
                throw std::runtime_error("Shape argument must follow --profile");
            auto& profile = result.profiles.back();
            if (profile.supplied[selector])
                throw std::runtime_error("Duplicate profile shape argument");
            profile.shapes[selector] = parse_shape(value);
            profile.supplied[selector] = true;
        }
    }
    if (result.onnx.empty() || result.engine.empty())
        throw std::runtime_error("Both --onnx and --saveEngine are required");
    if (result.profiles.size() != 3 && result.profiles.size() != 4)
        throw std::runtime_error("RIFE requires three or four complete profiles");
    for (const auto& profile : result.profiles) {
        if (profile.supplied != std::array<bool, 3>{true, true, true})
            throw std::runtime_error("Each profile requires min, opt and max shapes");
        for (int dimension = 2; dimension < 4; ++dimension) {
            if (profile.shapes[0].d[dimension] > profile.shapes[1].d[dimension]
                || profile.shapes[1].d[dimension] > profile.shapes[2].d[dimension])
                throw std::runtime_error("Profile shapes must satisfy min <= opt <= max");
        }
    }
    if (!std::filesystem::is_regular_file(result.onnx))
        throw std::runtime_error("ONNX input file does not exist");
    // The host owns temporary-file cleanup and atomic cache publication.
    if (std::filesystem::exists(result.engine))
        throw std::runtime_error("Refusing to overwrite an existing Engine");
    return result;
}

class Logger final : public nvinfer1::ILogger {
public:
    void log(Severity severity, const char* message) noexcept override
    {
        if (severity <= Severity::kWARNING)
            std::fprintf(stderr, "RIFE_ENGINE_BUILDER_TRT severity=%d %s\n",
                         static_cast<int>(severity), message);
    }
};

void build_engine(const Arguments& arguments)
{
    std::ifstream model(arguments.onnx, std::ios::binary | std::ios::ate);
    if (!model)
        throw std::runtime_error("Could not open ONNX input");
    const auto size = model.tellg();
    if (size <= 0 || size > 256 * 1024 * 1024)
        throw std::runtime_error("ONNX input is empty or exceeds 256 MiB");
    std::vector<char> bytes(static_cast<size_t>(size));
    model.seekg(0);
    if (!model.read(bytes.data(), static_cast<std::streamsize>(bytes.size())))
        throw std::runtime_error("Could not read the complete ONNX input");

    Logger logger;
    const std::unique_ptr<nvinfer1::IBuilder> builder(nvinfer1::createInferBuilder(logger));
    if (!builder)
        throw std::runtime_error("Could not create TensorRT-RTX builder");
    const auto flags = 1U << static_cast<uint32_t>(
        nvinfer1::NetworkDefinitionCreationFlag::kSTRONGLY_TYPED);
    const std::unique_ptr<nvinfer1::INetworkDefinition> network(builder->createNetworkV2(flags));
    const std::unique_ptr<nvinfer1::IBuilderConfig> config(builder->createBuilderConfig());
    if (!network || !config)
        throw std::runtime_error("Could not create TensorRT-RTX network/configuration");
    const std::unique_ptr<nvonnxparser::IParser> parser(nvonnxparser::createParser(*network, logger));
    if (!parser)
        throw std::runtime_error("Could not create ONNX parser");
    // Models are self-contained; read through the wide filesystem path rather
    // than the SDK's narrow parseFromFile API (Windows user paths may be Chinese).
    if (!parser->parse(bytes.data(), bytes.size())) {
        for (int index = 0; index < parser->getNbErrors(); ++index) {
            const auto* error = parser->getError(index);
            if (error)
                std::fprintf(stderr, "RIFE_ENGINE_BUILDER_ONNX_ERROR %s\n", error->desc());
        }
        throw std::runtime_error("ONNX parsing failed");
    }
    if (network->getNbInputs() != 1 || network->getNbOutputs() != 1)
        throw std::runtime_error("RIFE must have exactly one input and one output");
    const auto* input = network->getInput(0);
    const auto* output = network->getOutput(0);
    const auto input_shape = input->getDimensions();
    const auto output_shape = output->getDimensions();
    if (std::string_view(input->getName()) != "input"
        || input->getType() != nvinfer1::DataType::kHALF
        || output->getType() != nvinfer1::DataType::kHALF
        || input_shape.nbDims != 4 || input_shape.d[0] != 1 || input_shape.d[1] != 11
        || input_shape.d[2] != -1 || input_shape.d[3] != -1
        || output_shape.nbDims != 4 || output_shape.d[0] != 1 || output_shape.d[1] != 3)
        throw std::runtime_error("ONNX does not satisfy the dynamic FP16 RIFE I/O contract");

    // Matches the previous SDK CLI's --useGpu setting. Do not enable kFP16:
    // the frozen ONNX graph itself defines FP16/FP32 precision boundaries.
    if (!config->setNbComputeCapabilities(1)
        || !config->setComputeCapability(nvinfer1::ComputeCapability::kCURRENT, 0))
        throw std::runtime_error("Could not select the current GPU compute capability");
    // Freeze the previous SDK CLI defaults rather than relying on API defaults
    // (the SDK header documents only one average timing iteration).
    config->setAvgTimingIterations(8);
    config->setBuilderOptimizationLevel(3);
    config->setProfilingVerbosity(nvinfer1::ProfilingVerbosity::kLAYER_NAMES_ONLY);
    if (config->getAvgTimingIterations() != 8 || config->getBuilderOptimizationLevel() != 3
        || config->getProfilingVerbosity() != nvinfer1::ProfilingVerbosity::kLAYER_NAMES_ONLY)
        throw std::runtime_error("TensorRT did not accept the frozen builder configuration");
    std::printf("RIFE_ENGINE_BUILD_CONFIG avgTiming=%d optimizationLevel=%d profiling=layer_names_only\n",
                config->getAvgTimingIterations(), config->getBuilderOptimizationLevel());
    std::fflush(stdout);
    for (size_t index = 0; index < arguments.profiles.size(); ++index) {
        // The builder owns optimization profiles; callers must not delete them.
        auto* profile = builder->createOptimizationProfile();
        if (!profile)
            throw std::runtime_error("Could not create an optimization profile");
        for (int selector = 0; selector < 3; ++selector) {
            if (!profile->setDimensions("input", static_cast<nvinfer1::OptProfileSelector>(selector),
                                        arguments.profiles[index].shapes[selector]))
                throw std::runtime_error("TensorRT rejected a profile shape");
        }
        if (!profile->isValid() || config->addOptimizationProfile(profile) != static_cast<int>(index))
            throw std::runtime_error("TensorRT rejected the ordered profile contract");
    }
    const std::unique_ptr<nvinfer1::IHostMemory> engine(builder->buildSerializedNetwork(*network, *config));
    if (!engine || engine->size() == 0)
        throw std::runtime_error("TensorRT engine serialization failed");
    std::ofstream destination(arguments.engine, std::ios::binary);
    if (!destination)
        throw std::runtime_error("Could not create Engine output");
    destination.write(static_cast<const char*>(engine->data()), static_cast<std::streamsize>(engine->size()));
    destination.close();
    if (!destination)
        throw std::runtime_error("Could not write the complete Engine output");
    std::printf("RIFE_ENGINE_BUILD_OK tensorrt=1.4.0.76 profiles=%zu bytes=%zu gpu=current\n",
                arguments.profiles.size(), engine->size());
}

} // namespace

int wmain(int argc, wchar_t** argv)
{
    try {
        build_engine(parse_arguments(argc, argv));
        return 0;
    } catch (const std::exception& error) {
        std::fprintf(stderr, "RIFE_ENGINE_BUILD_FAILED %s\n", error.what());
        return 1;
    }
}
