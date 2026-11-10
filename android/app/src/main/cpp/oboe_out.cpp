// Oboe output for Airwave: same ring-buffer model as the OpenSL ES path, but
// Oboe picks the backend (AAudio on API 27+, OpenSL ES below) and a low-latency
// path when the device offers one. Kotlin writes into the ring; Oboe's callback
// drains it.

#include <jni.h>
#include <oboe/Oboe.h>
#include <pthread.h>
#include <cstdint>
#include <cstring>
#include <ctime>

class OboeOut : public oboe::AudioStreamDataCallback {
public:
    OboeOut(int rate, int channels, int bufferFrames)
        : channels_(channels), gain_(1.f), closed_(false) {
        pthread_mutex_init(&lock_, nullptr);
        pthread_cond_init(&space_, nullptr);
        int frames = bufferFrames > rate / 50 ? bufferFrames : rate / 50;
        ring_cap_ = frames * channels * 2;
        ring_ = new int16_t[ring_cap_]();
        ring_head_ = 0;
        ring_count_ = 0;
        rate_ = rate;
    }

    ~OboeOut() override {
        pthread_mutex_lock(&lock_);
        closed_ = true;
        pthread_cond_broadcast(&space_);
        pthread_mutex_unlock(&lock_);
        if (stream_) {
            stream_->stop();
            stream_->close();
            stream_.reset();
        }
        delete[] ring_;
        pthread_cond_destroy(&space_);
        pthread_mutex_destroy(&lock_);
    }

    bool start() {
        oboe::AudioStreamBuilder b;
        b.setDirection(oboe::Direction::Output)
            ->setPerformanceMode(oboe::PerformanceMode::LowLatency)
            ->setSharingMode(oboe::SharingMode::Shared)
            ->setFormat(oboe::AudioFormat::I16)
            ->setChannelCount(channels_)
            ->setSampleRate(rate_)
            ->setUsage(oboe::Usage::Media)
            ->setContentType(oboe::ContentType::Music)
            ->setDataCallback(this);
        oboe::Result r = b.openStream(stream_);
        if (r != oboe::Result::OK) return false;
        // Aim the device buffer at twice the burst size to cut glitches.
        stream_->setBufferSizeInFrames(stream_->getFramesPerBurst() * 2);
        if (stream_->requestStart() != oboe::Result::OK) {
            stream_->close();
            stream_.reset();
            return false;
        }
        return true;
    }

    int write(const int16_t *data, int count) {
        int done = 0;
        while (done < count) {
            pthread_mutex_lock(&lock_);
            while (!closed_ && ring_cap_ - ring_count_ == 0) {
                struct timespec ts;
                clock_gettime(CLOCK_REALTIME, &ts);
                ts.tv_nsec += 200 * 1000000L;
                if (ts.tv_nsec >= 1000000000L) {
                    ts.tv_sec += 1;
                    ts.tv_nsec -= 1000000000L;
                }
                pthread_cond_timedwait(&space_, &lock_, &ts);
            }
            if (closed_) {
                pthread_mutex_unlock(&lock_);
                break;
            }
            int space = ring_cap_ - ring_count_;
            int k = count - done;
            if (k > space) k = space;
            int tail = (ring_head_ + ring_count_) % ring_cap_;
            for (int i = 0; i < k; i++) {
                ring_[tail] = data[done + i];
                tail = (tail + 1) % ring_cap_;
            }
            ring_count_ += k;
            pthread_mutex_unlock(&lock_);
            done += k;
        }
        return done;
    }

    void setGain(float g) {
        pthread_mutex_lock(&lock_);
        gain_ = g;
        pthread_mutex_unlock(&lock_);
    }

    oboe::DataCallbackResult onAudioReady(oboe::AudioStream *, void *audioData, int32_t numFrames) override {
        auto *out = static_cast<int16_t *>(audioData);
        int wanted = numFrames * channels_;
        pthread_mutex_lock(&lock_);
        int n = ring_count_ < wanted ? ring_count_ : wanted;
        float g = gain_;
        for (int i = 0; i < n; i++) {
            float v = ring_[ring_head_] * g;
            if (v > 32767.f) v = 32767.f;
            if (v < -32768.f) v = -32768.f;
            out[i] = (int16_t) v;
            ring_head_ = (ring_head_ + 1) % ring_cap_;
        }
        ring_count_ -= n;
        pthread_cond_signal(&space_);
        pthread_mutex_unlock(&lock_);
        if (n < wanted) memset(out + n, 0, (size_t) (wanted - n) * sizeof(int16_t));
        return oboe::DataCallbackResult::Continue;
    }

private:
    std::shared_ptr<oboe::AudioStream> stream_;
    int channels_;
    int rate_;
    int16_t *ring_;
    int ring_cap_;   // samples
    int ring_head_;  // next sample to play
    int ring_count_; // samples waiting
    pthread_mutex_t lock_;
    pthread_cond_t space_;
    float gain_;
    bool closed_;
};

extern "C" {

JNIEXPORT jlong JNICALL
Java_com_airwave_client_NativeOboe_create(JNIEnv *env, jobject thiz, jint rate, jint channels, jint buffer_frames) {
    (void) env;
    (void) thiz;
    if (rate <= 0 || channels < 1 || channels > 2) return 0;
    auto *o = new OboeOut(rate, channels, buffer_frames);
    if (!o->start()) {
        delete o;
        return 0;
    }
    return (jlong) (intptr_t) o;
}

JNIEXPORT jint JNICALL
Java_com_airwave_client_NativeOboe_write(JNIEnv *env, jobject thiz, jlong handle, jshortArray data, jint count) {
    (void) thiz;
    auto *o = (OboeOut *) (intptr_t) handle;
    if (!o) return 0;
    jshort tmp[2048];
    int done = 0;
    while (done < count) {
        int k = count - done;
        if (k > 2048) k = 2048;
        env->GetShortArrayRegion(data, done, k, tmp);
        int w = o->write(tmp, k);
        done += w;
        if (w < k) break; // stream closed
    }
    return done;
}

JNIEXPORT void JNICALL
Java_com_airwave_client_NativeOboe_setGain(JNIEnv *env, jobject thiz, jlong handle, jfloat gain) {
    (void) env;
    (void) thiz;
    auto *o = (OboeOut *) (intptr_t) handle;
    if (o) o->setGain(gain);
}

JNIEXPORT void JNICALL
Java_com_airwave_client_NativeOboe_destroy(JNIEnv *env, jobject thiz, jlong handle) {
    (void) env;
    (void) thiz;
    delete (OboeOut *) (intptr_t) handle;
}

} // extern "C"
