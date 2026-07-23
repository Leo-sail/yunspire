#ifndef NOMINMAX
#define NOMINMAX
#endif
#include <windows.h>
#include <wincodec.h>

#include <algorithm>
#include <cmath>
#include <cstdint>
#include <cwchar>
#include <filesystem>
#include <fstream>
#include <iomanip>
#include <iostream>
#include <limits>
#include <sstream>
#include <stdexcept>
#include <string>

#include <wrl/client.h>

namespace fs = std::filesystem;
using Microsoft::WRL::ComPtr;

namespace {

struct ImageResult {
    std::uint32_t source_width{};
    std::uint32_t source_height{};
    std::uint32_t output_width{};
    std::uint32_t output_height{};
    std::uint64_t byte_length{};
    bool derived{};
    std::string path;
};

std::string utf8(const std::wstring& value) {
    if (value.empty()) return {};
    const int size = WideCharToMultiByte(
        CP_UTF8, WC_ERR_INVALID_CHARS, value.data(), static_cast<int>(value.size()),
        nullptr, 0, nullptr, nullptr);
    if (size <= 0) throw std::runtime_error("utf8_conversion_failed");
    std::string output(static_cast<std::size_t>(size), '\0');
    if (WideCharToMultiByte(
            CP_UTF8, WC_ERR_INVALID_CHARS, value.data(), static_cast<int>(value.size()),
            output.data(), size, nullptr, nullptr) != size) {
        throw std::runtime_error("utf8_conversion_failed");
    }
    return output;
}

std::string json_escape(const std::string& value) {
    std::ostringstream output;
    for (const unsigned char character : value) {
        switch (character) {
            case '\"': output << "\\\""; break;
            case '\\': output << "\\\\"; break;
            case '\b': output << "\\b"; break;
            case '\f': output << "\\f"; break;
            case '\n': output << "\\n"; break;
            case '\r': output << "\\r"; break;
            case '\t': output << "\\t"; break;
            default:
                if (character < 0x20) {
                    output << "\\u" << std::hex << std::setw(4) << std::setfill('0')
                           << static_cast<int>(character) << std::dec;
                } else {
                    output << character;
                }
        }
    }
    return output.str();
}

std::string hresult_code(HRESULT result) {
    std::ostringstream output;
    output << "0x" << std::hex << std::uppercase << static_cast<std::uint32_t>(result);
    return output.str();
}

void require(HRESULT result, const char* operation) {
    if (FAILED(result)) {
        throw std::runtime_error(std::string(operation) + ':' + hresult_code(result));
    }
}

void emit(const ImageResult* result, const std::string& error = {}) {
    std::ostringstream output;
    output << "{\"schema\":\"yunspire.windows-image-derivative.v1\","
           << "\"encoder\":\"Windows Imaging Component\","
           << "\"mime_type\":\"image/jpeg\",";
    if (result) {
        output << "\"source_width\":" << result->source_width << ','
               << "\"source_height\":" << result->source_height << ','
               << "\"output_width\":" << result->output_width << ','
               << "\"output_height\":" << result->output_height << ','
               << "\"byte_length\":" << result->byte_length << ','
               << "\"derived\":" << (result->derived ? "true" : "false") << ','
               << "\"path\":\"" << json_escape(result->path) << "\",";
    } else {
        output << "\"source_width\":null,\"source_height\":null,"
               << "\"output_width\":null,\"output_height\":null,"
               << "\"byte_length\":null,\"derived\":false,\"path\":null,";
    }
    output << "\"errors\":[";
    if (!error.empty()) output << '\"' << json_escape(error) << '\"';
    output << "]}";
    std::cout << output.str();
}

std::uint64_t checked_pixel_bytes(
    std::uint32_t width,
    std::uint32_t height,
    std::uint64_t bytes_per_pixel) {
    if (width == 0 || height == 0) throw std::runtime_error("image_dimensions_invalid");
    const auto pixels = static_cast<std::uint64_t>(width) * height;
    if (pixels > std::numeric_limits<std::uint64_t>::max() / bytes_per_pixel) {
        throw std::runtime_error("image_dimensions_overflow");
    }
    return pixels * bytes_per_pixel;
}

void ensure_resource_budget(
    std::uint32_t source_width,
    std::uint32_t source_height,
    std::uint32_t output_width,
    std::uint32_t output_height,
    const fs::path& destination) {
    const auto source_bytes = checked_pixel_bytes(source_width, source_height, 4);
    const auto output_bytes = checked_pixel_bytes(output_width, output_height, 4);
    const auto working_bytes = std::max(source_bytes, output_bytes);

    MEMORYSTATUSEX memory{};
    memory.dwLength = sizeof(memory);
    if (!GlobalMemoryStatusEx(&memory)) {
        throw std::runtime_error("image_resource_memory_query_failed");
    }
    const auto usable_memory = memory.ullAvailPhys - memory.ullAvailPhys / 3;
    if (working_bytes > usable_memory) {
        throw std::runtime_error("image_resource_memory_insufficient");
    }

    ULARGE_INTEGER available{};
    const auto parent = destination.parent_path();
    if (!GetDiskFreeSpaceExW(parent.c_str(), &available, nullptr, nullptr)) {
        throw std::runtime_error("image_resource_disk_query_failed");
    }
    const auto usable_disk = available.QuadPart - available.QuadPart / 10;
    const auto conservative_output_bytes = checked_pixel_bytes(output_width, output_height, 3);
    if (conservative_output_bytes > usable_disk) {
        throw std::runtime_error("image_resource_disk_insufficient");
    }
}

std::pair<std::uint32_t, std::uint32_t> target_dimensions(
    std::uint32_t width,
    std::uint32_t height,
    std::uint32_t maximum_edge) {
    const auto source_edge = std::max(width, height);
    if (source_edge <= maximum_edge) return {width, height};
    const double scale = static_cast<double>(maximum_edge) / source_edge;
    return {
        std::max<std::uint32_t>(1, static_cast<std::uint32_t>(std::llround(width * scale))),
        std::max<std::uint32_t>(1, static_cast<std::uint32_t>(std::llround(height * scale))),
    };
}

ComPtr<IWICBitmapFrameDecode> open_frame(
    IWICImagingFactory* factory,
    const fs::path& path) {
    ComPtr<IWICBitmapDecoder> decoder;
    require(
        factory->CreateDecoderFromFilename(
            path.c_str(), nullptr, GENERIC_READ, WICDecodeMetadataCacheOnDemand, &decoder),
        "image_decoder_open_failed");
    UINT frame_count = 0;
    require(decoder->GetFrameCount(&frame_count), "image_frame_count_failed");
    if (frame_count == 0) throw std::runtime_error("image_has_no_frames");
    ComPtr<IWICBitmapFrameDecode> frame;
    require(decoder->GetFrame(0, &frame), "image_frame_open_failed");
    return frame;
}

void encode_jpeg(
    IWICImagingFactory* factory,
    IWICBitmapSource* source,
    std::uint32_t width,
    std::uint32_t height,
    const fs::path& destination) {
    ComPtr<IWICStream> stream;
    require(factory->CreateStream(&stream), "image_output_stream_create_failed");
    require(
        stream->InitializeFromFilename(destination.c_str(), GENERIC_WRITE),
        "image_output_stream_open_failed");
    ComPtr<IWICBitmapEncoder> encoder;
    require(
        factory->CreateEncoder(GUID_ContainerFormatJpeg, nullptr, &encoder),
        "image_jpeg_encoder_create_failed");
    require(encoder->Initialize(stream.Get(), WICBitmapEncoderNoCache), "image_encoder_init_failed");

    ComPtr<IWICBitmapFrameEncode> frame;
    ComPtr<IPropertyBag2> properties;
    require(encoder->CreateNewFrame(&frame, &properties), "image_encoder_frame_create_failed");
    PROPBAG2 option{};
    option.pstrName = const_cast<LPOLESTR>(L"ImageQuality");
    VARIANT quality;
    VariantInit(&quality);
    quality.vt = VT_R4;
    quality.fltVal = 0.9F;
    require(properties->Write(1, &option, &quality), "image_encoder_quality_failed");
    require(frame->Initialize(properties.Get()), "image_encoder_frame_init_failed");
    require(frame->SetSize(width, height), "image_encoder_size_failed");
    WICPixelFormatGUID format = GUID_WICPixelFormat24bppBGR;
    require(frame->SetPixelFormat(&format), "image_encoder_pixel_format_failed");
    if (format != GUID_WICPixelFormat24bppBGR) {
        throw std::runtime_error("image_encoder_pixel_format_changed");
    }
    require(frame->WriteSource(source, nullptr), "image_encoder_write_failed");
    require(frame->Commit(), "image_encoder_frame_commit_failed");
    require(encoder->Commit(), "image_encoder_commit_failed");
}

std::uint64_t verify_jpeg(
    IWICImagingFactory* factory,
    const fs::path& destination,
    std::uint32_t expected_width,
    std::uint32_t expected_height) {
    const auto length = fs::file_size(destination);
    if (length < 4) throw std::runtime_error("image_output_empty");
    std::ifstream input(destination, std::ios::binary);
    unsigned char prefix[2]{};
    unsigned char suffix[2]{};
    input.read(reinterpret_cast<char*>(prefix), 2);
    input.seekg(-2, std::ios::end);
    input.read(reinterpret_cast<char*>(suffix), 2);
    if (!input || prefix[0] != 0xFF || prefix[1] != 0xD8
        || suffix[0] != 0xFF || suffix[1] != 0xD9) {
        throw std::runtime_error("image_output_jpeg_signature_invalid");
    }
    const auto frame = open_frame(factory, destination);
    UINT width = 0;
    UINT height = 0;
    require(frame->GetSize(&width, &height), "image_output_dimensions_failed");
    if (width != expected_width || height != expected_height) {
        throw std::runtime_error("image_output_dimensions_mismatch");
    }
    return length;
}

ImageResult derive(
    IWICImagingFactory* factory,
    const fs::path& source,
    const fs::path& destination,
    std::uint32_t maximum_edge) {
    if (!fs::is_regular_file(source)) throw std::runtime_error("image_source_not_found");
    std::error_code directory_error;
    fs::create_directories(destination.parent_path(), directory_error);
    if (directory_error || !fs::is_directory(destination.parent_path())) {
        throw std::runtime_error("image_output_directory_unavailable");
    }
    std::error_code equivalent_error;
    if (fs::equivalent(source, destination, equivalent_error) && !equivalent_error) {
        throw std::runtime_error("image_source_equals_destination");
    }

    const auto frame = open_frame(factory, source);
    UINT source_width = 0;
    UINT source_height = 0;
    require(frame->GetSize(&source_width, &source_height), "image_dimensions_failed");
    const auto [output_width, output_height] = target_dimensions(
        source_width, source_height, maximum_edge);
    ensure_resource_budget(
        source_width, source_height, output_width, output_height, destination);

    ComPtr<IWICBitmapSource> scaled;
    if (output_width != source_width || output_height != source_height) {
        ComPtr<IWICBitmapScaler> scaler;
        require(factory->CreateBitmapScaler(&scaler), "image_scaler_create_failed");
        require(
            scaler->Initialize(
                frame.Get(), output_width, output_height, WICBitmapInterpolationModeFant),
            "image_scaler_init_failed");
        require(scaler.As(&scaled), "image_scaler_interface_failed");
    } else {
        require(frame.As(&scaled), "image_frame_interface_failed");
    }

    ComPtr<IWICFormatConverter> converter;
    require(factory->CreateFormatConverter(&converter), "image_converter_create_failed");
    require(
        converter->Initialize(
            scaled.Get(), GUID_WICPixelFormat24bppBGR,
            WICBitmapDitherTypeNone, nullptr, 0.0, WICBitmapPaletteTypeCustom),
        "image_converter_init_failed");
    encode_jpeg(factory, converter.Get(), output_width, output_height, destination);
    const auto bytes = verify_jpeg(factory, destination, output_width, output_height);
    return ImageResult{
        source_width,
        source_height,
        output_width,
        output_height,
        bytes,
        output_width != source_width || output_height != source_height,
        utf8(fs::absolute(destination).wstring()),
    };
}

}  // namespace

int wmain(int argc, wchar_t* argv[]) {
    if (argc < 4) {
        emit(nullptr, "usage:yunspire_image_windows.exe <source> <target.jpg> <maximum-edge>");
        return 0;
    }

    wchar_t* end = nullptr;
    const auto parsed_edge = std::wcstoull(argv[3], &end, 10);
    if (!end || *end != L'\0' || parsed_edge == 0
        || parsed_edge > std::numeric_limits<std::uint32_t>::max()) {
        emit(nullptr, "image_maximum_edge_invalid");
        return 0;
    }

    const HRESULT apartment = CoInitializeEx(nullptr, COINIT_MULTITHREADED);
    if (FAILED(apartment) && apartment != RPC_E_CHANGED_MODE) {
        emit(nullptr, "image_com_initialization_failed:" + hresult_code(apartment));
        return 0;
    }
    try {
        ComPtr<IWICImagingFactory> factory;
        require(
            CoCreateInstance(
                CLSID_WICImagingFactory, nullptr, CLSCTX_INPROC_SERVER,
                IID_PPV_ARGS(&factory)),
            "image_wic_factory_failed");
        const auto result = derive(
            factory.Get(), fs::absolute(fs::path(argv[1])),
            fs::absolute(fs::path(argv[2])),
            static_cast<std::uint32_t>(parsed_edge));
        emit(&result);
    } catch (const std::exception& error) {
        std::error_code ignored;
        fs::remove(fs::path(argv[2]), ignored);
        emit(nullptr, error.what());
    }
    if (SUCCEEDED(apartment)) CoUninitialize();
    return 0;
}
