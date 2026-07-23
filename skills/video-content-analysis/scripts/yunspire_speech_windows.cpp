#define WIN32_LEAN_AND_MEAN
#ifndef NOMINMAX
#define NOMINMAX
#endif
#include <windows.h>
#include <sapi.h>
#include <sphelper.h>

#include <algorithm>
#include <cstdint>
#include <cstring>
#include <cwchar>
#include <filesystem>
#include <fstream>
#include <iostream>
#include <limits>
#include <sstream>
#include <string>
#include <vector>

#pragma comment(lib, "sapi.lib")
#pragma comment(lib, "ole32.lib")

template <typename T>
class ComPtr {
 public:
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

struct Segment { uint64_t start_ms; uint64_t end_ms; std::string text; };

namespace fs = std::filesystem;

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
      default: if (character < 0x20) out << " "; else out << character;
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

static uint64_t wav_duration_ms(const std::wstring& path) {
  std::ifstream stream(fs::path(path), std::ios::binary);
  char header[44] = {};
  if (!stream.read(header, sizeof(header)) || std::string(header, header + 4) != "RIFF" || std::string(header + 8, header + 12) != "WAVE") return 0;
  uint32_t bytes_per_second = 0;
  uint32_t data_size = 0;
  memcpy(&bytes_per_second, header + 28, sizeof(bytes_per_second));
  memcpy(&data_size, header + 40, sizeof(data_size));
  return bytes_per_second ? static_cast<uint64_t>(data_size) * 1000 / bytes_per_second : 0;
}

static void emit(const std::string& locale, const std::vector<Segment>& segments,
                 bool on_device, const std::vector<std::string>& warnings, const std::vector<std::string>& errors) {
  std::ostringstream transcript;
  bool first_segment = true;
  for (const Segment& segment : segments) {
    if (!first_segment) transcript << ' ';
    transcript << segment.text;
    first_segment = false;
  }
  std::cout << "{\"schema\":\"yunspire.windows-speech.v1\",\"transcript\":\"" << json_escape(transcript.str()) << "\",\"locale\":\"" << json_escape(locale)
            << "\",\"on_device\":" << (on_device ? "true" : "false") << ",\"segments\":[";
  for (size_t index = 0; index < segments.size(); ++index) {
    if (index) std::cout << ',';
    const Segment& segment = segments[index];
    std::cout << "{\"start_ms\":" << segment.start_ms << ",\"end_ms\":" << segment.end_ms
              << ",\"text\":\"" << json_escape(segment.text) << "\",\"confidence\":0.0}";
  }
  std::cout << "],\"warnings\":[";
  for (size_t index = 0; index < warnings.size(); ++index) { if (index) std::cout << ','; std::cout << '\"' << json_escape(warnings[index]) << '\"'; }
  std::cout << "],\"errors\":[";
  for (size_t index = 0; index < errors.size(); ++index) { if (index) std::cout << ','; std::cout << '\"' << json_escape(errors[index]) << '\"'; }
  std::cout << "]}";
}

static bool normalize_locale(const std::wstring& requested, std::wstring* canonical, LANGID* language) {
  wchar_t resolved[LOCALE_NAME_MAX_LENGTH] = {};
  const int length = ResolveLocaleName(requested.c_str(), resolved, LOCALE_NAME_MAX_LENGTH);
  if (length <= 0) return false;
  const LCID locale_id = LocaleNameToLCID(resolved, 0);
  if (!locale_id) return false;
  *canonical = resolved;
  *language = LANGIDFROMLCID(locale_id);
  return true;
}

static bool token_matches_language(ISpObjectToken* token, LANGID requested_language) {
  LANGID primary_language = 0;
  if (SUCCEEDED(SpGetLanguageFromToken(token, &primary_language)) && primary_language == requested_language) return true;
  LPWSTR languages = nullptr;
  if (FAILED(token->GetStringValue(L"Language", &languages)) || !languages) return false;
  bool matched = false;
  std::wstring values = languages;
  CoTaskMemFree(languages);
  size_t start = 0;
  while (start <= values.size()) {
    const size_t end = values.find(L';', start);
    const std::wstring item = values.substr(start, end == std::wstring::npos ? std::wstring::npos : end - start);
    wchar_t* parsed_end = nullptr;
    const unsigned long token_language = wcstoul(item.c_str(), &parsed_end, 16);
    if (parsed_end != item.c_str() && *parsed_end == L'\0'
        && token_language == static_cast<unsigned long>(requested_language)) {
      matched = true;
      break;
    }
    if (end == std::wstring::npos) break;
    start = end + 1;
  }
  return matched;
}

static HRESULT matching_recognizer_token(LANGID requested_language, ISpObjectToken** result) {
  if (!result) return E_POINTER;
  *result = nullptr;
  ComPtr<IEnumSpObjectTokens> tokens;
  HRESULT hr = SpEnumTokens(SPCAT_RECOGNIZERS, nullptr, nullptr, tokens.Put());
  if (FAILED(hr)) return hr;
  for (;;) {
    ComPtr<ISpObjectToken> token;
    ULONG fetched = 0;
    hr = tokens->Next(1, token.Put(), &fetched);
    if (hr == S_FALSE || fetched == 0) return S_FALSE;
    if (FAILED(hr)) return hr;
    if (token_matches_language(token.Get(), requested_language)) {
      *result = token.Get();
      (*result)->AddRef();
      return S_OK;
    }
  }
}

int wmain(int argc, wchar_t** argv) {
  const std::wstring requested_locale = argc >= 3 ? argv[2] : L"zh-CN";
  std::wstring canonical_locale;
  LANGID requested_language = 0;
  if (!normalize_locale(requested_locale, &canonical_locale, &requested_language)) {
    emit(utf8(requested_locale), {}, false, {"无法规范化请求的 Windows 语音识别语言"}, {"windows_sapi_language_unavailable"}); return 0;
  }
  const std::string locale = utf8(canonical_locale);
  if (argc < 2) { emit(locale, {}, false, {}, {"audio_path_missing"}); return 0; }
  const std::wstring audio = argv[1];
  if (!wav_duration_ms(audio)) { emit(locale, {}, false, {"Windows 本地语音识别只接受云枢 Media Foundation 输出的 PCM WAV 音频"}, {"windows_sapi_audio_unavailable"}); return 0; }
  HRESULT apartment = CoInitializeEx(nullptr, COINIT_MULTITHREADED);
  if (FAILED(apartment) && apartment != RPC_E_CHANGED_MODE) { emit(locale, {}, false, {"Windows COM 初始化失败"}, {"windows_sapi_unavailable"}); return 0; }
  ComApartment apartment_scope(apartment);
  ComPtr<ISpRecognizer> recognizer;
  HRESULT hr = CoCreateInstance(CLSID_SpInprocRecognizer, nullptr, CLSCTX_INPROC_SERVER, IID_PPV_ARGS(recognizer.Put()));
  if (FAILED(hr)) { emit(locale, {}, false, {"Windows 未安装可用的本地 SAPI 语音识别引擎"}, {"windows_sapi_recognizer_unavailable"}); return 0; }
  ComPtr<ISpObjectToken> token;
  hr = matching_recognizer_token(requested_language, token.Put());
  if (hr != S_OK || FAILED(recognizer->SetRecognizer(token.Get()))) {
    emit(locale, {}, false, {"未安装与请求语言精确匹配的 Windows 本地 SAPI 识别器"}, {"windows_sapi_language_unavailable"}); return 0;
  }
  ComPtr<ISpStream> stream;
  hr = SPBindToFile(audio.c_str(), SPFM_OPEN_READONLY, stream.Put());
  if (FAILED(hr) || FAILED(recognizer->SetInput(stream.Get(), TRUE))) { emit(locale, {}, false, {"无法将本地 WAV 音频交给 Windows SAPI"}, {"windows_sapi_input_unavailable"}); return 0; }
  ComPtr<ISpRecoContext> context;
  ComPtr<ISpRecoGrammar> grammar;
  hr = recognizer->CreateRecoContext(context.Put());
  const ULONGLONG event_interest = SPFEI(SPEI_RECOGNITION) | SPFEI(SPEI_FALSE_RECOGNITION) | SPFEI(SPEI_END_SR_STREAM);
  if (FAILED(hr) || FAILED(context->SetNotifyWin32Event()) || FAILED(context->SetInterest(event_interest, event_interest)) || FAILED(context->CreateGrammar(1, grammar.Put())) || FAILED(grammar->LoadDictation(nullptr, SPLO_STATIC)) || FAILED(grammar->SetDictationState(SPRS_ACTIVE)) || FAILED(recognizer->SetRecoState(SPRST_ACTIVE))) {
    emit(locale, {}, false, {"Windows SAPI 本地听写语法不可用；请安装与系统语言匹配的离线语音识别功能"}, {"windows_sapi_dictation_unavailable"}); return 0;
  }
  progress_checkpoint("windows-speech-started");
  std::vector<Segment> segments;
  bool stream_ended = false;
  ULONGLONG last_stream_position = 0;
  ULONGLONG last_checkpoint = GetTickCount64();
  while (!stream_ended) {
    if (context->WaitForNotifyEvent(250) == S_OK) {
      SPEVENT event = {};
      ULONG fetched = 0;
      while (SUCCEEDED(context->GetEvents(1, &event, &fetched)) && fetched) {
        if (event.eEventId == SPEI_RECOGNITION && event.lParam) {
          ISpRecoResult* result = reinterpret_cast<ISpRecoResult*>(event.lParam);
          LPWSTR text = nullptr;
          constexpr ULONG whole_phrase = static_cast<ULONG>(SP_GETWHOLEPHRASE);
          if (SUCCEEDED(result->GetText(whole_phrase, whole_phrase, TRUE, &text, nullptr)) && text) {
            SPRECORESULTTIMES times = {};
            if (SUCCEEDED(result->GetResultTimes(&times))) {
              segments.push_back({times.ullStart / 10000, (times.ullStart + times.ullLength) / 10000, utf8(text)});
              progress_checkpoint("windows-speech-recognized:" + std::to_string(times.ullStart / 10000));
              last_checkpoint = GetTickCount64();
            }
            CoTaskMemFree(text);
          }
        } else if (event.eEventId == SPEI_FALSE_RECOGNITION) {
          progress_checkpoint("windows-speech-rejected");
          last_checkpoint = GetTickCount64();
        } else if (event.eEventId == SPEI_END_SR_STREAM) {
          stream_ended = true;
        }
        SpClearEvent(&event);
        fetched = 0;
      }
    }
    LARGE_INTEGER zero = {};
    ULARGE_INTEGER position = {};
    const ULONGLONG now = GetTickCount64();
    if (now - last_checkpoint >= 5000
        && SUCCEEDED(stream->Seek(zero, STREAM_SEEK_CUR, &position))
        && position.QuadPart > last_stream_position) {
      last_stream_position = position.QuadPart;
      progress_checkpoint("windows-speech-read:" + std::to_string(last_stream_position));
      last_checkpoint = now;
    }
  }
  grammar->SetDictationState(SPRS_INACTIVE);
  recognizer->SetRecoState(SPRST_INACTIVE);
  std::vector<std::string> warnings;
  std::vector<std::string> errors;
  if (segments.empty()) { warnings.push_back("Windows SAPI 未返回可用转写；请确认已安装匹配语言的离线语音识别功能"); errors.push_back("windows_sapi_transcript_unavailable"); }
  progress_checkpoint("windows-speech-finished:" + std::to_string(segments.size()));
  emit(locale, segments, true, warnings, errors);
  return 0;
}
