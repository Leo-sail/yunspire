#define WIN32_LEAN_AND_MEAN
#ifndef NOMINMAX
#define NOMINMAX
#endif
#include <windows.h>
#include <mfapi.h>
#include <mferror.h>
#include <mfidl.h>
#include <mfreadwrite.h>
#include <wincodec.h>
#include <oleauto.h>

#include <algorithm>
#include <cmath>
#include <cstdint>
#include <cstdio>
#include <cstring>
#include <filesystem>
#include <fstream>
#include <iomanip>
#include <iostream>
#include <sstream>
#include <limits>
#include <string>
#include <vector>

#pragma comment(lib, "mfplat.lib")
#pragma comment(lib, "mfreadwrite.lib")
#pragma comment(lib, "mfuuid.lib")
#pragma comment(lib, "windowscodecs.lib")
#pragma comment(lib, "ole32.lib")
#pragma comment(lib, "oleaut32.lib")

template <typename T>
class ComPtr {
 public:
  ComPtr() = default;
  ~ComPtr() { Reset(); }
  T** Put() { Reset(); return &value_; }
  T* Get() const { return value_; }
  T* operator->() const { return value_; }
  void Reset() { if (value_) { value_->Release(); value_ = nullptr; } }
 private:
  T* value_ = nullptr;
};

class ComApartment {
 public:
  explicit ComApartment(HRESULT result) : initialized_(SUCCEEDED(result)) {}
  ~ComApartment() { if (initialized_) CoUninitialize(); }
  ComApartment(const ComApartment&) = delete;
  ComApartment& operator=(const ComApartment&) = delete;
 private:
  bool initialized_ = false;
};

class MediaFoundationScope {
 public:
  explicit MediaFoundationScope(HRESULT result) : initialized_(SUCCEEDED(result)) {}
  ~MediaFoundationScope() { if (initialized_) MFShutdown(); }
  MediaFoundationScope(const MediaFoundationScope&) = delete;
  MediaFoundationScope& operator=(const MediaFoundationScope&) = delete;
 private:
  bool initialized_ = false;
};

namespace fs = std::filesystem;

constexpr DWORD kMediaSourceStream = static_cast<DWORD>(MF_SOURCE_READER_MEDIASOURCE);
constexpr DWORD kFirstVideoStream = static_cast<DWORD>(MF_SOURCE_READER_FIRST_VIDEO_STREAM);
constexpr DWORD kFirstAudioStream = static_cast<DWORD>(MF_SOURCE_READER_FIRST_AUDIO_STREAM);

static std::string utf8(const std::wstring& value) {
  if (value.empty()) return "";
  if (value.size() > static_cast<size_t>(std::numeric_limits<int>::max())) return "";
  const int input_size = static_cast<int>(value.size());
  const int size = WideCharToMultiByte(CP_UTF8, WC_ERR_INVALID_CHARS, value.data(), input_size, nullptr, 0, nullptr, nullptr);
  if (size <= 0) return "";
  std::string result(static_cast<size_t>(size), '\0');
  if (WideCharToMultiByte(CP_UTF8, WC_ERR_INVALID_CHARS, value.data(), input_size, result.data(), size, nullptr, nullptr) != size) return "";
  return result;
}

static std::string json_escape(const std::string& value) {
  std::ostringstream out;
  for (unsigned char character : value) {
    switch (character) {
      case '\\': out << "\\\\"; break;
      case '"': out << "\\\""; break;
      case '\n': out << "\\n"; break;
      case '\r': out << "\\r"; break;
      case '\t': out << "\\t"; break;
      default:
        if (character < 0x20) out << "\\u" << std::hex << std::setw(4) << std::setfill('0') << static_cast<int>(character) << std::dec;
        else out << character;
    }
  }
  return out.str();
}

static void progress_checkpoint(const std::string& label) {
  const DWORD required = GetEnvironmentVariableW(L"YUNSPIRE_PROGRESS_FILE", nullptr, 0);
  if (required == 0) return;
  std::vector<wchar_t> path(required);
  const DWORD copied = GetEnvironmentVariableW(
    L"YUNSPIRE_PROGRESS_FILE", path.data(), static_cast<DWORD>(path.size()));
  if (copied == 0 || copied >= path.size()) return;
  std::ofstream stream(fs::path(path.data()), std::ios::binary | std::ios::app);
  if (stream) stream << GetTickCount64() << '\t' << label << '\n';
}

static void emit_failure(const std::string& warning, const std::string& error) {
  std::cout << "{\"schema\":\"yunspire.windows-media.v1\",\"duration_seconds\":0,\"audio_path\":\"\",\"frames\":[],\"frame_timestamps_ms\":[],\"frame_difference_scores\":[],\"frame_candidate_count\":0,\"frame_selection_method\":\"yunspire-windows-mediafoundation-v1\",\"warnings\":[\""
            << json_escape(warning) << "\"],\"errors\":[\"" << json_escape(error) << "\"]}";
}

static HRESULT create_reader(const std::wstring& path, bool video, IMFSourceReader** result) {
  ComPtr<IMFAttributes> attributes;
  HRESULT hr = MFCreateAttributes(attributes.Put(), 2);
  if (FAILED(hr)) return hr;
  if (video) attributes->SetUINT32(MF_SOURCE_READER_ENABLE_VIDEO_PROCESSING, TRUE);
  return MFCreateSourceReaderFromURL(path.c_str(), attributes.Get(), result);
}

static bool codec_unavailable_hresult(HRESULT result) {
  return result == MF_E_TOPO_CODEC_NOT_FOUND
    || result == MF_E_INVALIDMEDIATYPE
    || result == MF_E_TRANSFORM_TYPE_NOT_SET;
}

static double media_duration_seconds(const std::wstring& path) {
  ComPtr<IMFSourceReader> reader;
  if (FAILED(create_reader(path, false, reader.Put()))) return 0.0;
  PROPVARIANT duration;
  PropVariantInit(&duration);
  const HRESULT hr = reader->GetPresentationAttribute(kMediaSourceStream, MF_PD_DURATION, &duration);
  const double seconds = SUCCEEDED(hr) && duration.vt == VT_UI8
    ? static_cast<double>(duration.uhVal.QuadPart) / 10000000.0
    : 0.0;
  PropVariantClear(&duration);
  return std::isfinite(seconds) && seconds > 0.0 ? seconds : 0.0;
}

static bool source_has_stream(const std::wstring& path, DWORD stream_index, const GUID& expected_major_type) {
  ComPtr<IMFSourceReader> reader;
  if (FAILED(create_reader(path, false, reader.Put()))) return false;
  ComPtr<IMFMediaType> media_type;
  if (FAILED(reader->GetNativeMediaType(stream_index, 0, media_type.Put()))) return false;
  GUID major_type = GUID_NULL;
  return SUCCEEDED(media_type->GetGUID(MF_MT_MAJOR_TYPE, &major_type))
    && IsEqualGUID(major_type, expected_major_type);
}

static std::vector<BYTE> resize_for_model(const std::vector<BYTE>& pixels, UINT32 width, UINT32 height, UINT32* output_width, UINT32* output_height) {
  constexpr UINT32 kMaximumModelEdge = 960;
  const double scale = std::max(width, height) > kMaximumModelEdge
    ? static_cast<double>(kMaximumModelEdge) / static_cast<double>(std::max(width, height))
    : 1.0;
  *output_width = std::max<UINT32>(1, static_cast<UINT32>(std::lround(width * scale)));
  *output_height = std::max<UINT32>(1, static_cast<UINT32>(std::lround(height * scale)));
  std::vector<BYTE> output(static_cast<size_t>(*output_width) * *output_height * 3);
  for (UINT32 y = 0; y < *output_height; ++y) {
    const UINT32 source_y = std::min(height - 1, static_cast<UINT32>((static_cast<uint64_t>(y) * height) / *output_height));
    for (UINT32 x = 0; x < *output_width; ++x) {
      const UINT32 source_x = std::min(width - 1, static_cast<UINT32>((static_cast<uint64_t>(x) * width) / *output_width));
      const BYTE* source = &pixels[(static_cast<size_t>(source_y) * width + source_x) * 4];
      BYTE* target = &output[(static_cast<size_t>(y) * *output_width + x) * 3];
      target[0] = source[0]; target[1] = source[1]; target[2] = source[2];
    }
  }
  return output;
}

static bool write_model_jpeg(const std::wstring& output, UINT32 width, UINT32 height, const std::vector<BYTE>& pixels) {
  UINT32 encoded_width = 0, encoded_height = 0;
  const std::vector<BYTE> encoded = resize_for_model(pixels, width, height, &encoded_width, &encoded_height);
  ComPtr<IWICImagingFactory> factory;
  if (FAILED(CoCreateInstance(CLSID_WICImagingFactory, nullptr, CLSCTX_INPROC_SERVER, IID_PPV_ARGS(factory.Put())))) return false;
  ComPtr<IWICStream> stream;
  if (FAILED(factory->CreateStream(stream.Put())) || FAILED(stream->InitializeFromFilename(output.c_str(), GENERIC_WRITE))) return false;
  ComPtr<IWICBitmapEncoder> encoder;
  if (FAILED(factory->CreateEncoder(GUID_ContainerFormatJpeg, nullptr, encoder.Put())) || FAILED(encoder->Initialize(stream.Get(), WICBitmapEncoderNoCache))) return false;
  ComPtr<IWICBitmapFrameEncode> frame;
  ComPtr<IPropertyBag2> properties;
  if (FAILED(encoder->CreateNewFrame(frame.Put(), properties.Put()))) return false;
  PROPBAG2 quality = {};
  quality.pstrName = const_cast<LPOLESTR>(L"ImageQuality");
  VARIANT quality_value;
  VariantInit(&quality_value);
  quality_value.vt = VT_R4;
  quality_value.fltVal = 0.78f;
  properties->Write(1, &quality, &quality_value);
  if (FAILED(frame->Initialize(properties.Get())) || FAILED(frame->SetSize(encoded_width, encoded_height))) return false;
  GUID pixel_format = GUID_WICPixelFormat24bppBGR;
  if (FAILED(frame->SetPixelFormat(&pixel_format)) || pixel_format != GUID_WICPixelFormat24bppBGR) return false;
  const UINT stride = encoded_width * 3;
  if (FAILED(frame->WritePixels(encoded_height, stride, static_cast<UINT>(encoded.size()), const_cast<BYTE*>(encoded.data())))) return false;
  return SUCCEEDED(frame->Commit()) && SUCCEEDED(encoder->Commit());
}

struct FrameMetrics {
  std::vector<BYTE> fingerprint;
  double luminance = 0.0;
  double deviation = 0.0;
};

static FrameMetrics measure_frame(const std::vector<BYTE>& pixels, UINT32 width, UINT32 height) {
  FrameMetrics metrics;
  metrics.fingerprint.reserve(256);
  double sum = 0.0;
  for (UINT32 y = 0; y < 16; ++y) {
    UINT32 source_y = std::min(height - 1, static_cast<UINT32>((static_cast<uint64_t>(y) * height) / 16));
    for (UINT32 x = 0; x < 16; ++x) {
      UINT32 source_x = std::min(width - 1, static_cast<UINT32>((static_cast<uint64_t>(x) * width) / 16));
      const BYTE* pixel = &pixels[(static_cast<size_t>(source_y) * width + source_x) * 4];
      BYTE luminance = static_cast<BYTE>((11 * pixel[0] + 59 * pixel[1] + 30 * pixel[2]) / 100);
      metrics.fingerprint.push_back(luminance);
      sum += luminance;
    }
  }
  metrics.luminance = sum / static_cast<double>(metrics.fingerprint.size());
  double variance = 0.0;
  for (BYTE value : metrics.fingerprint) variance += (value - metrics.luminance) * (value - metrics.luminance);
  metrics.deviation = std::sqrt(variance / static_cast<double>(metrics.fingerprint.size()));
  return metrics;
}

static double frame_difference(const std::vector<BYTE>& before, const std::vector<BYTE>& after) {
  if (before.empty() || after.empty() || before.size() != after.size()) return 255.0;
  double total = 0.0;
  for (size_t index = 0; index < before.size(); ++index) total += std::abs(static_cast<int>(before[index]) - static_cast<int>(after[index]));
  return total / static_cast<double>(before.size());
}

static bool extract_video_frames(const std::wstring& path, const std::wstring& output_directory,
                                 std::vector<std::wstring>* frames, std::vector<int64_t>* timestamps,
                                 std::vector<double>* differences, uint64_t* candidates,
                                 std::vector<std::string>* warnings, bool* codec_unavailable) {
  ComPtr<IMFSourceReader> reader;
  HRESULT hr = create_reader(path, true, reader.Put());
  if (FAILED(hr)) {
    *codec_unavailable = codec_unavailable_hresult(hr);
    warnings->push_back("Windows Media Foundation 无法打开视频流");
    return false;
  }
  if (FAILED(reader->SetStreamSelection(kFirstAudioStream, FALSE))) {
    warnings->push_back("无法禁用视频读取器的音轨");
  }
  ComPtr<IMFMediaType> requested;
  if (FAILED(MFCreateMediaType(requested.Put()))) { warnings->push_back("无法创建视频解码类型"); return false; }
  if (FAILED(requested->SetGUID(MF_MT_MAJOR_TYPE, MFMediaType_Video))
      || FAILED(requested->SetGUID(MF_MT_SUBTYPE, MFVideoFormat_RGB32))) {
    warnings->push_back("无法配置视频解码类型"); return false;
  }
  hr = reader->SetCurrentMediaType(kFirstVideoStream, nullptr, requested.Get());
  if (FAILED(hr)) {
    *codec_unavailable = true;
    warnings->push_back("Windows Media Foundation 缺少可将该视频流解码为 RGB32 的本地编解码器");
    return false;
  }
  ComPtr<IMFMediaType> current;
  UINT32 width = 0, height = 0;
  if (FAILED(reader->GetCurrentMediaType(kFirstVideoStream, current.Put())) || FAILED(MFGetAttributeSize(current.Get(), MF_MT_FRAME_SIZE, &width, &height)) || width == 0 || height == 0) {
    warnings->push_back("无法读取视频画面尺寸"); return false;
  }
  UINT32 raw_stride = MFGetAttributeUINT32(current.Get(), MF_MT_DEFAULT_STRIDE, width * 4);
  const LONG stride = static_cast<LONG>(raw_stride);
  const LONGLONG candidate_interval = 2 * 10 * 1000 * 1000;
  LONGLONG next_candidate = 0;
  std::vector<BYTE> previous;
  bool has_video = false;
  bool completed = true;
  ULONGLONG last_checkpoint = GetTickCount64();
  for (;;) {
    DWORD flags = 0;
    LONGLONG timestamp = 0;
    ComPtr<IMFSample> sample;
    hr = reader->ReadSample(kFirstVideoStream, 0, nullptr, &flags, &timestamp, sample.Put());
    if (FAILED(hr)) {
      *codec_unavailable = *codec_unavailable || codec_unavailable_hresult(hr);
      warnings->push_back("视频解码在读取画面时失败"); completed = false; break;
    }
    if (flags & MF_SOURCE_READERF_ERROR) { warnings->push_back("视频解码器报告流错误"); completed = false; break; }
    if (flags & MF_SOURCE_READERF_ENDOFSTREAM) break;
    if (flags & MF_SOURCE_READERF_CURRENTMEDIATYPECHANGED) {
      warnings->push_back("视频中途改变画面格式，已停止提取以避免生成错误画面"); completed = false; break;
    }
    if (sample.Get() == nullptr || timestamp < next_candidate) continue;
    next_candidate = timestamp + candidate_interval;
    ++*candidates;
    const ULONGLONG now = GetTickCount64();
    if (now - last_checkpoint >= 5000) {
      progress_checkpoint("windows-video-decoded:" + std::to_string(timestamp / 10000));
      last_checkpoint = now;
    }
    ComPtr<IMFMediaBuffer> buffer;
    if (FAILED(sample->ConvertToContiguousBuffer(buffer.Put()))) continue;
    BYTE* data = nullptr;
    DWORD maximum = 0, length = 0;
    const HRESULT lock_result = buffer->Lock(&data, &maximum, &length);
    if (FAILED(lock_result)) continue;
    if (!data) { buffer->Unlock(); continue; }
    const uint64_t required_bytes = static_cast<uint64_t>(width) * height * 4;
    if (required_bytes > std::numeric_limits<size_t>::max()) { buffer->Unlock(); warnings->push_back("视频画面尺寸超过本机可处理范围"); completed = false; break; }
    const size_t required = static_cast<size_t>(required_bytes);
    std::vector<BYTE> pixels(required);
    const uint64_t absolute_stride = stride < 0
      ? static_cast<uint64_t>(-static_cast<int64_t>(stride))
      : static_cast<uint64_t>(stride);
    if (absolute_stride < static_cast<uint64_t>(width) * 4 || absolute_stride * height > length) { buffer->Unlock(); continue; }
    for (UINT32 row = 0; row < height; ++row) {
      const UINT32 source_row = stride < 0 ? height - 1 - row : row;
      memcpy(&pixels[static_cast<size_t>(row) * width * 4], data + static_cast<size_t>(source_row) * static_cast<size_t>(absolute_stride), static_cast<size_t>(width) * 4);
    }
    buffer->Unlock();
    has_video = true;
    FrameMetrics metrics = measure_frame(pixels, width, height);
    double difference = frame_difference(previous, metrics.fingerprint);
    bool visible = metrics.luminance >= 4.0 && metrics.luminance <= 251.0 && metrics.deviation >= 4.0;
    if (!visible || (!previous.empty() && difference < 9.0)) continue;
    std::wostringstream name;
    name << output_directory << L"\\frame-" << std::setfill(L'0') << std::setw(6) << (frames->size() + 1) << L".jpg";
    _wremove(name.str().c_str());
    if (write_model_jpeg(name.str(), width, height, pixels)) {
      frames->push_back(name.str());
      timestamps->push_back(timestamp / 10000);
      differences->push_back(difference);
      previous = std::move(metrics.fingerprint);
    } else {
      _wremove(name.str().c_str());
      warnings->push_back("关键帧 JPEG 写入失败，已停止提取并保留此前结果");
      completed = false;
      break;
    }
  }
  progress_checkpoint("windows-video-finished:" + std::to_string(*candidates));
  if (!has_video) warnings->push_back("媒体没有可解码的视频画面");
  return has_video && completed;
}

#pragma pack(push, 1)
struct WaveHeader {
  char riff[4] = {'R','I','F','F'}; uint32_t file_size = 0; char wave[4] = {'W','A','V','E'};
  char fmt[4] = {'f','m','t',' '}; uint32_t fmt_size = 16; uint16_t format = 1;
  uint16_t channels = 1; uint32_t samples_per_second = 16000; uint32_t bytes_per_second = 32000;
  uint16_t block_align = 2; uint16_t bits_per_sample = 16; char data[4] = {'d','a','t','a'}; uint32_t data_size = 0;
};
#pragma pack(pop)

static bool extract_audio(const std::wstring& path, const std::wstring& output,
                          std::wstring* audio_path, std::vector<std::string>* warnings,
                          bool* codec_unavailable) {
  ComPtr<IMFSourceReader> reader;
  HRESULT hr = create_reader(path, false, reader.Put());
  if (FAILED(hr)) {
    *codec_unavailable = codec_unavailable_hresult(hr);
    warnings->push_back("Windows Media Foundation 无法打开音轨");
    return false;
  }
  if (FAILED(reader->SetStreamSelection(kFirstVideoStream, FALSE))) {
    warnings->push_back("无法禁用音轨读取器的视频流");
  }
  ComPtr<IMFMediaType> requested;
  if (FAILED(MFCreateMediaType(requested.Put()))) return false;
  if (FAILED(requested->SetGUID(MF_MT_MAJOR_TYPE, MFMediaType_Audio))
      || FAILED(requested->SetGUID(MF_MT_SUBTYPE, MFAudioFormat_PCM))
      || FAILED(requested->SetUINT32(MF_MT_AUDIO_BITS_PER_SAMPLE, 16))
      || FAILED(requested->SetUINT32(MF_MT_AUDIO_SAMPLES_PER_SECOND, 16000))
      || FAILED(requested->SetUINT32(MF_MT_AUDIO_NUM_CHANNELS, 1))) {
    warnings->push_back("无法配置本地 PCM 音轨格式"); return false;
  }
  if (FAILED(reader->SetCurrentMediaType(kFirstAudioStream, nullptr, requested.Get()))) {
    requested->DeleteAllItems();
    requested->SetGUID(MF_MT_MAJOR_TYPE, MFMediaType_Audio);
    requested->SetGUID(MF_MT_SUBTYPE, MFAudioFormat_PCM);
    hr = reader->SetCurrentMediaType(kFirstAudioStream, nullptr, requested.Get());
    if (FAILED(hr)) {
      *codec_unavailable = true;
      warnings->push_back("Windows Media Foundation 缺少可将该音轨解码为 PCM 的本地编解码器");
      return false;
    }
    warnings->push_back("音轨保留原始 PCM 采样率和声道数供本地语音识别使用");
  }
  ComPtr<IMFMediaType> current;
  UINT32 rate = 0, channels = 0, bits = 0, media_block_align = 0;
  if (FAILED(reader->GetCurrentMediaType(kFirstAudioStream, current.Put()))) return false;
  if (FAILED(current->GetUINT32(MF_MT_AUDIO_SAMPLES_PER_SECOND, &rate))
      || FAILED(current->GetUINT32(MF_MT_AUDIO_NUM_CHANNELS, &channels))
      || FAILED(current->GetUINT32(MF_MT_AUDIO_BITS_PER_SAMPLE, &bits))
      || FAILED(current->GetUINT32(MF_MT_AUDIO_BLOCK_ALIGNMENT, &media_block_align))
      || bits != 16 || rate == 0 || channels == 0) {
    warnings->push_back("Windows 音轨解码未返回完整的 16 位 PCM 参数"); return false;
  }
  WaveHeader header;
  header.channels = static_cast<uint16_t>(channels);
  header.samples_per_second = rate;
  header.bits_per_sample = static_cast<uint16_t>(bits);
  const uint64_t block_align = static_cast<uint64_t>(channels) * bits / 8;
  if (block_align == 0 || block_align > std::numeric_limits<uint16_t>::max()
      || block_align != media_block_align
      || static_cast<uint64_t>(rate) * block_align > std::numeric_limits<uint32_t>::max()) {
    warnings->push_back("Windows 音轨 PCM 参数超出 WAV 可表示范围"); return false;
  }
  header.block_align = static_cast<uint16_t>(block_align);
  header.bytes_per_second = rate * header.block_align;
  std::ofstream stream(fs::path(output), std::ios::binary | std::ios::trunc);
  if (!stream) { warnings->push_back("无法创建本地语音 WAV 文件"); return false; }
  stream.write(reinterpret_cast<const char*>(&header), sizeof(header));
  uint64_t pcm_size = 0;
  ULONGLONG last_checkpoint = GetTickCount64();
  for (;;) {
    DWORD flags = 0;
    ComPtr<IMFSample> sample;
    hr = reader->ReadSample(kFirstAudioStream, 0, nullptr, &flags, nullptr, sample.Put());
    if (FAILED(hr)) {
      *codec_unavailable = *codec_unavailable || codec_unavailable_hresult(hr);
      stream.close(); _wremove(output.c_str()); warnings->push_back("音轨解码在读取样本时失败"); return false;
    }
    if (flags & MF_SOURCE_READERF_ERROR) { stream.close(); _wremove(output.c_str()); warnings->push_back("音轨解码器报告流错误"); return false; }
    if (flags & MF_SOURCE_READERF_ENDOFSTREAM) break;
    if (flags & MF_SOURCE_READERF_CURRENTMEDIATYPECHANGED) { stream.close(); _wremove(output.c_str()); warnings->push_back("音轨中途改变格式，无法生成单一 PCM WAV"); return false; }
    if (sample.Get() == nullptr) continue;
    ComPtr<IMFMediaBuffer> buffer;
    if (FAILED(sample->ConvertToContiguousBuffer(buffer.Put()))) continue;
    BYTE* data = nullptr;
    DWORD maximum = 0, length = 0;
    const HRESULT lock_result = buffer->Lock(&data, &maximum, &length);
    if (FAILED(lock_result)) continue;
    if (data && length) {
      if (pcm_size + length > std::numeric_limits<uint32_t>::max() - 36ULL) {
        buffer->Unlock(); stream.close(); _wremove(output.c_str());
        warnings->push_back("本地 PCM 音轨超过标准 WAV 的 4GB 容量"); return false;
      }
      stream.write(reinterpret_cast<const char*>(data), static_cast<std::streamsize>(length));
      pcm_size += length;
      const ULONGLONG now = GetTickCount64();
      if (now - last_checkpoint >= 5000) {
        progress_checkpoint("windows-audio-decoded:" + std::to_string(pcm_size));
        last_checkpoint = now;
      }
    }
    buffer->Unlock();
    if (!stream) { stream.close(); _wremove(output.c_str()); warnings->push_back("写入本地语音 WAV 文件失败"); return false; }
  }
  if (pcm_size == 0) { stream.close(); _wremove(output.c_str()); warnings->push_back("媒体没有可导出的音轨"); return false; }
  header.data_size = static_cast<uint32_t>(pcm_size);
  header.file_size = 36 + header.data_size;
  stream.seekp(0, std::ios::beg);
  stream.write(reinterpret_cast<const char*>(&header), sizeof(header));
  stream.flush();
  if (!stream.good()) { stream.close(); _wremove(output.c_str()); warnings->push_back("写入本地语音 WAV 文件失败"); return false; }
  *audio_path = output;
  progress_checkpoint("windows-audio-finished:" + std::to_string(pcm_size));
  return true;
}

int wmain(int argc, wchar_t** argv) {
  if (argc < 3) { emit_failure("Windows 媒体适配器缺少参数", "media_arguments_missing"); return 0; }
  HRESULT apartment = CoInitializeEx(nullptr, COINIT_MULTITHREADED);
  if (FAILED(apartment) && apartment != RPC_E_CHANGED_MODE) { emit_failure("Windows COM 初始化失败", "windows_com_unavailable"); return 0; }
  ComApartment apartment_scope(apartment);
  HRESULT startup = MFStartup(MF_VERSION);
  if (FAILED(startup)) { emit_failure("Windows Media Foundation 初始化失败", "windows_mediafoundation_unavailable"); return 0; }
  MediaFoundationScope media_foundation_scope(startup);
  progress_checkpoint("windows-media-started");
  const std::wstring media = argv[1];
  const std::wstring output = argv[2];
  std::error_code directory_error;
  fs::create_directories(fs::path(output), directory_error);
  if (directory_error || !fs::is_directory(fs::path(output))) { emit_failure("无法创建媒体输出目录", "media_output_directory_unavailable"); return 0; }
  if (!fs::is_regular_file(fs::path(media))) { emit_failure("本地媒体文件不存在", "media_source_unavailable"); return 0; }
  std::vector<std::wstring> frames;
  std::vector<int64_t> timestamps;
  std::vector<double> differences;
  std::vector<std::string> warnings;
  uint64_t candidates = 0;
  const double duration_seconds = media_duration_seconds(media);
  const bool video_stream_present = source_has_stream(media, kFirstVideoStream, MFMediaType_Video);
  const bool audio_stream_present = source_has_stream(media, kFirstAudioStream, MFMediaType_Audio);
  bool video_codec_unavailable = false;
  bool audio_codec_unavailable = false;
  const bool video_extraction_completed = !video_stream_present || extract_video_frames(
    media, output, &frames, &timestamps, &differences, &candidates, &warnings,
    &video_codec_unavailable);
  std::wstring audio;
  const bool audio_extraction_completed = !audio_stream_present || extract_audio(
    media, output + L"\\speech-audio.wav", &audio, &warnings, &audio_codec_unavailable);
  std::vector<std::string> errors;
  if (video_codec_unavailable || audio_codec_unavailable) errors.push_back("windows_media_codec_unavailable");
  if (video_stream_present && (!video_extraction_completed || frames.empty())) errors.push_back("windows_video_frames_unavailable");
  if (audio_stream_present && (!audio_extraction_completed || audio.empty())) errors.push_back("windows_audio_extraction_unavailable");
  if (!video_stream_present && !audio_stream_present) errors.push_back("windows_media_source_invalid");
  else if (frames.empty() && audio.empty()) errors.push_back("windows_media_content_unavailable");
  std::cout << "{\"schema\":\"yunspire.windows-media.v1\",\"duration_seconds\":" << std::fixed << std::setprecision(3) << duration_seconds
            << ",\"video_stream_present\":" << (video_stream_present ? "true" : "false")
            << ",\"audio_stream_present\":" << (audio_stream_present ? "true" : "false")
            << ",\"audio_path\":\"" << json_escape(utf8(audio)) << "\",\"frames\":[";
  for (size_t index = 0; index < frames.size(); ++index) { if (index) std::cout << ','; std::cout << '\"' << json_escape(utf8(frames[index])) << '\"'; }
  std::cout << "],\"frame_timestamps_ms\":[";
  for (size_t index = 0; index < timestamps.size(); ++index) { if (index) std::cout << ','; std::cout << timestamps[index]; }
  std::cout << "],\"frame_difference_scores\":[";
  for (size_t index = 0; index < differences.size(); ++index) { if (index) std::cout << ','; std::cout << std::fixed << std::setprecision(2) << differences[index]; }
  std::cout << "],\"frame_candidate_count\":" << candidates << ",\"frame_selection_method\":\"yunspire-windows-mediafoundation-v1\",\"warnings\":[";
  for (size_t index = 0; index < warnings.size(); ++index) { if (index) std::cout << ','; std::cout << '\"' << json_escape(warnings[index]) << '\"'; }
  std::cout << "],\"errors\":[";
  for (size_t index = 0; index < errors.size(); ++index) { if (index) std::cout << ','; std::cout << '\"' << json_escape(errors[index]) << '\"'; }
  std::cout << "]}";
  progress_checkpoint("windows-media-finished");
  return 0;
}
