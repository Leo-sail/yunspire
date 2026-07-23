#ifndef NOMINMAX
#define NOMINMAX
#endif
#include <windows.h>

#include <algorithm>
#include <cmath>
#include <cstdint>
#include <filesystem>
#include <fstream>
#include <iomanip>
#include <iostream>
#include <sstream>
#include <stdexcept>
#include <string>
#include <tuple>
#include <vector>

#include <winrt/Windows.Data.Pdf.h>
#include <winrt/Windows.Foundation.h>
#include <winrt/Windows.Graphics.Imaging.h>
#include <winrt/Windows.Storage.h>
#include <winrt/Windows.Storage.Streams.h>
#include <winrt/Windows.UI.h>
#include <winrt/base.h>

namespace fs = std::filesystem;
using winrt::Windows::Data::Pdf::PdfDocument;
using winrt::Windows::Data::Pdf::PdfPage;
using winrt::Windows::Data::Pdf::PdfPageRenderOptions;
using winrt::Windows::Graphics::Imaging::BitmapEncoder;
using winrt::Windows::Storage::StorageFile;
using winrt::Windows::Storage::Streams::DataReader;
using winrt::Windows::Storage::Streams::InMemoryRandomAccessStream;

namespace {

constexpr double kPdfPointsPerInch = 72.0;
constexpr double kTargetDpi = 144.0;
constexpr std::uint32_t kInitialLongEdgePixels = 2200;
constexpr std::uint32_t kMinimumLongEdgePixels = 768;
constexpr std::uint64_t kModelReadyPageBytes = 5ULL * 1024ULL * 1024ULL / 2ULL;
constexpr std::uint32_t kStreamChunkBytes = 1024U * 1024U;

struct PageResult {
    std::uint32_t page_number{};
    double width_points{};
    double height_points{};
    std::uint32_t render_width{};
    std::uint32_t render_height{};
    std::uint64_t byte_length{};
    std::string path;
    std::string name;
    bool resolution_reduced{};
};

std::string utf8(const std::wstring& value) {
    return winrt::to_string(winrt::hstring(value));
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

std::string hresult_code(const winrt::hresult_error& error) {
    std::ostringstream output;
    output << "0x" << std::hex << std::uppercase
           << static_cast<std::uint32_t>(error.code().value);
    return output.str();
}

void emit_result(
    std::uint32_t page_count,
    const std::vector<PageResult>& pages,
    const std::vector<std::string>& warnings,
    const std::vector<std::string>& errors) {
    std::ostringstream output;
    output << std::setprecision(10);
    output << "{\"schema\":\"yunspire.windows-pdf.v1\","
           << "\"renderer\":\"Windows.Data.Pdf\","
           << "\"image_format\":\"image/jpeg\","
           << "\"page_count\":" << page_count << ','
           << "\"rendered_page_count\":" << pages.size() << ','
           << "\"pages\":[";
    for (std::size_t index = 0; index < pages.size(); ++index) {
        if (index) output << ',';
        const auto& page = pages[index];
        output << "{\"page_number\":" << page.page_number
               << ",\"width_points\":" << page.width_points
               << ",\"height_points\":" << page.height_points
               << ",\"render_width\":" << page.render_width
               << ",\"render_height\":" << page.render_height
               << ",\"byte_length\":" << page.byte_length
               << ",\"resolution_reduced\":"
               << (page.resolution_reduced ? "true" : "false")
               << ",\"name\":\"" << json_escape(page.name) << "\""
               << ",\"path\":\"" << json_escape(page.path) << "\"}";
    }
    output << "],\"warnings\":[";
    for (std::size_t index = 0; index < warnings.size(); ++index) {
        if (index) output << ',';
        output << '\"' << json_escape(warnings[index]) << '\"';
    }
    output << "],\"errors\":[";
    for (std::size_t index = 0; index < errors.size(); ++index) {
        if (index) output << ',';
        output << '\"' << json_escape(errors[index]) << '\"';
    }
    output << "]}";
    std::cout << output.str();
}

std::pair<std::uint32_t, std::uint32_t> render_dimensions(
    double width_points,
    double height_points,
    std::uint32_t long_edge_limit) {
    const double base_scale = kTargetDpi / kPdfPointsPerInch;
    double width = std::max(1.0, width_points * base_scale);
    double height = std::max(1.0, height_points * base_scale);
    const double long_edge = std::max(width, height);
    if (long_edge > static_cast<double>(long_edge_limit)) {
        const double scale = static_cast<double>(long_edge_limit) / long_edge;
        width *= scale;
        height *= scale;
    }
    return {
        std::max<std::uint32_t>(1, static_cast<std::uint32_t>(std::llround(width))),
        std::max<std::uint32_t>(1, static_cast<std::uint32_t>(std::llround(height))),
    };
}

std::uint64_t write_stream(
    InMemoryRandomAccessStream const& stream,
    const fs::path& destination) {
    stream.Seek(0);
    const auto input = stream.GetInputStreamAt(0);
    DataReader reader(input);
    std::ofstream output(destination, std::ios::binary | std::ios::trunc);
    if (!output) throw std::runtime_error("pdf_page_output_open_failed");

    std::uint64_t written = 0;
    while (written < stream.Size()) {
        const auto request = static_cast<std::uint32_t>(
            std::min<std::uint64_t>(kStreamChunkBytes, stream.Size() - written));
        const auto loaded = reader.LoadAsync(request).get();
        if (loaded == 0) break;
        std::vector<std::uint8_t> bytes(loaded);
        reader.ReadBytes(bytes);
        output.write(
            reinterpret_cast<const char*>(bytes.data()),
            static_cast<std::streamsize>(bytes.size()));
        if (!output) throw std::runtime_error("pdf_page_output_write_failed");
        written += bytes.size();
    }
    output.flush();
    if (!output || written != stream.Size()) {
        throw std::runtime_error("pdf_page_output_incomplete");
    }
    return written;
}

std::uint64_t render_page(
    PdfPage const& page,
    const fs::path& destination,
    std::uint32_t width,
    std::uint32_t height) {
    PdfPageRenderOptions options;
    options.DestinationWidth(width);
    options.DestinationHeight(height);
    options.BitmapEncoderId(BitmapEncoder::JpegEncoderId());
    options.BackgroundColor(winrt::Windows::UI::Colors::White());
    InMemoryRandomAccessStream stream;
    page.RenderToStreamAsync(stream, options).get();
    return write_stream(stream, destination);
}

PageResult render_model_ready_page(
    PdfPage const& page,
    std::uint32_t page_number,
    const fs::path& output_directory) {
    const auto size = page.Size();
    if (!std::isfinite(size.Width) || !std::isfinite(size.Height)
        || size.Width <= 0 || size.Height <= 0) {
        throw std::runtime_error("pdf_page_dimensions_invalid");
    }
    std::ostringstream name;
    name << "pdf-page-" << std::setw(5) << std::setfill('0') << page_number << ".jpg";
    const fs::path destination = output_directory / fs::path(name.str());

    std::uint32_t long_edge = kInitialLongEdgePixels;
    auto [width, height] = render_dimensions(size.Width, size.Height, long_edge);
    std::uint64_t bytes = render_page(page, destination, width, height);
    bool reduced = false;
    while (bytes > kModelReadyPageBytes && long_edge > kMinimumLongEdgePixels) {
        reduced = true;
        const double ratio = std::sqrt(
            static_cast<double>(kModelReadyPageBytes) / static_cast<double>(bytes));
        const auto next_edge = static_cast<std::uint32_t>(std::floor(
            static_cast<double>(long_edge) * std::clamp(ratio * 0.9, 0.5, 0.85)));
        long_edge = std::max(kMinimumLongEdgePixels, next_edge);
        std::tie(width, height) = render_dimensions(size.Width, size.Height, long_edge);
        bytes = render_page(page, destination, width, height);
    }
    if (bytes == 0 || bytes > kModelReadyPageBytes) {
        std::error_code ignored;
        fs::remove(destination, ignored);
        throw std::runtime_error("pdf_page_model_image_budget_unavailable");
    }
    return PageResult{
        page_number,
        static_cast<double>(size.Width),
        static_cast<double>(size.Height),
        width,
        height,
        bytes,
        utf8(fs::absolute(destination).wstring()),
        name.str(),
        reduced,
    };
}

}  // namespace

int wmain(int argc, wchar_t* argv[]) {
    if (argc < 2) {
        emit_result(0, {}, {}, {"pdf_path_missing"});
        return 0;
    }
    if (argc < 3) {
        emit_result(0, {}, {}, {"pdf_output_directory_missing"});
        return 0;
    }

    std::vector<PageResult> pages;
    std::vector<std::string> warnings;
    std::vector<std::string> errors;
    std::uint32_t page_count = 0;
    bool apartment_initialized = false;
    try {
        const fs::path source = fs::absolute(fs::path(argv[1]));
        const fs::path output_directory = fs::absolute(fs::path(argv[2]));
        if (!fs::is_regular_file(source)) {
            emit_result(0, {}, {}, {"pdf_source_not_found"});
            return 0;
        }
        std::error_code directory_error;
        fs::create_directories(output_directory, directory_error);
        if (directory_error || !fs::is_directory(output_directory)) {
            emit_result(0, {}, {}, {"pdf_output_directory_unavailable"});
            return 0;
        }

        winrt::init_apartment(winrt::apartment_type::multi_threaded);
        apartment_initialized = true;
        const auto file = StorageFile::GetFileFromPathAsync(source.wstring()).get();
        const auto document = PdfDocument::LoadFromFileAsync(file).get();
        page_count = document.PageCount();
        if (page_count == 0) {
            errors.push_back("pdf_has_no_pages");
        } else {
            pages.reserve(page_count);
            for (std::uint32_t index = 0; index < page_count; ++index) {
                try {
                    const auto page = document.GetPage(index);
                    pages.push_back(render_model_ready_page(page, index + 1, output_directory));
                    if (pages.back().resolution_reduced) {
                        warnings.push_back(
                            "pdf_page_resolution_reduced_for_model_input:" +
                            std::to_string(index + 1));
                    }
                } catch (const winrt::hresult_error& error) {
                    errors.push_back(
                        "pdf_page_render_failed:" + std::to_string(index + 1) + ':' +
                        hresult_code(error) + ':' + winrt::to_string(error.message()));
                } catch (const std::exception& error) {
                    errors.push_back(
                        "pdf_page_render_failed:" + std::to_string(index + 1) + ':' + error.what());
                }
            }
            if (pages.size() != page_count) {
                errors.push_back("pdf_render_incomplete");
            }
        }
    } catch (const winrt::hresult_error& error) {
        errors.push_back(
            "pdf_open_failed:" + hresult_code(error) + ':' + winrt::to_string(error.message()));
    } catch (const std::exception& error) {
        errors.push_back(std::string("pdf_render_unavailable:") + error.what());
    }
    if (apartment_initialized) winrt::uninit_apartment();
    emit_result(page_count, pages, warnings, errors);
    return 0;
}
