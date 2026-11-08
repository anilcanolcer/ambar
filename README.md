<img src="assets/icon.svg" alt="Ambar logosu" width="96">

# 🏡 Ambar

Ambar; video ve ses bağlantılarını bilgisayarına kaydetmeye yönelik, Türkçe bir masaüstü uygulaması taslağıdır. Anılcan Ölçer tarafından, [Kova](https://github.com/alpunlu12-commits/kova) projesinin kaynak kodu temel alınarak kişiselleştirilmiştir. İndirme ve dönüşüm işlemleri yt-dlp, FFmpeg ve Deno gibi ayrı açık kaynak bileşenlerle yapılır. Yalnızca indirme hakkına sahip olduğun içerikleri kullan.

## Durum

Windows için deneme sürümü derlenmiştir. Serbest lisanslı bir örnekte MP3 indirme ve dönüşümü doğrulandı; video dönüşümündeki düzeltme yeni derlemede sınanmayı bekliyor. Kararlı sürüm henüz yayımlanmadı. Otomatik uygulama güncellemeleri güvenlik için devre dışıdır; Kova'nın sürümleri yüklenmez. Bileşen güncellemeleri ayrı çalışır.

## Geliştirme

Node.js ve npm, Rust ve Tauri 2'nin işletim sistemine özgü ön koşulları gereklidir. Windows'ta Visual Studio C++ Build Tools ve WebView2 gerekir. Ön koşullar için https://v2.tauri.app/start/prerequisites/ adresine bakın.

```sh
npm ci
npm run app
npm run app:build
```

Windows ve macOS yükleyicileri kendi platformlarında üretilip sınanmalıdır. GitHub'a yüklemek tek başına yükleyici veya güncelleme oluşturmaz.

## GitHub üzerinden kurulum dosyası ve güncellemeler

Henüz hazırlanmış bir yayın veya yükleyici yoktur.

1. Proje kökünde `npm ci` çalıştırın, ardından `npm run tauri signer generate -- -w ambar-signing.key` ile Tauri anahtar çiftini oluşturun. Özel anahtar dosyasını güvenli bir yerde yedekleyin; depoya veya sohbetlere göndermeyin. Onu kaybederseniz mevcut kurulumlara güncelleme gönderemezsiniz.
2. GitHub depo ayarlarında Actions değişkenlerine `AMBAR_UPDATER_PUBLIC_KEY` adıyla açık anahtarın içeriğini, Actions secrets bölümüne `TAURI_SIGNING_PRIVATE_KEY` adıyla özel anahtarın içeriğini ve şifre kullanıldıysa `TAURI_SIGNING_PRIVATE_KEY_PASSWORD` değerini ekleyin.
3. Actions sekmesinde **Ambar sürüm taslağı** iş akışını çalıştırın. Windows kurulum dosyası, macOS DMG ve imzalı güncelleme dosyaları taslak GitHub sürümüne eklenir. Bu akış henüz gerçek bir GitHub çalıştırmasıyla doğrulanmamıştır.
4. Her iki platformda kurulum, indirme ve bir sonraki sürüme yükseltme testlerini yapın. Taslak sürümü inceleyip yayımlayın. Sonraki sürümlerde `package.json`, `src-tauri/Cargo.toml` ve `src-tauri/tauri.conf.json` sürümlerini aynı yeni sürüme yükseltin.

Windows ve macOS uygulama imzalama sertifikaları ayrıca yapılandırılmadığından sistemler güvenlik uyarıları gösterebilir. Tauri güncelleme imzası bu sertifikaların yerine geçmez.

## Lisans ve kaynak

Ambar, [Alp'in Kova projesi](https://github.com/alpunlu12-commits/kova) temel alınarak hazırlanmıştır. Orijinal projenin telif bildirimi ve MIT lisansı [LICENSE](LICENSE) dosyasında korunmaktadır. MIT lisansı, lisans ve telif bildiriminin korunması koşuluyla kodun kullanılmasına, değiştirilmesine ve dağıtılmasına izin verir. Diğer bileşenlerin kendi lisansları geçerlidir.


## ✨ Taslakta yer alan özellikler

- 🔗 Bağlantıdan medya bilgilerini ve varsa kapak görselini önizleme
- 🎵 MP3, M4A, OPUS, FLAC ve WAV ses biçimleri
- 🎬 MP4, MKV ve WEBM video biçimleri
- 📚 Oynatma listesi seçimi, indirme kuyruğu ve geçmiş
- 📁 İndirme klasörü seçimi ve uygun videolarda altyazı seçenekleri
- 💻 Windows ve macOS için Tauri tabanlı masaüstü arayüzü

Bu özellikler Kova altyapısından devralınmıştır; Ambar sürümünde henüz uçtan uca doğrulanmamıştır. Destek, kaynak siteye ve seçilen içeriğe göre değişebilir.

## 🚀 Kullanım

Kurulabilir sürüm yayımlandığında Ambar’ı açın, indirme hakkına sahip olduğunuz içeriğin bağlantısını yapıştırın, **Getir** seçeneğini kullanın ve istediğiniz biçimi seçerek indirmeyi başlatın. İlk açılışta gerekli bileşenler için onay istenebilir.

## 🖼️ Ekran görüntüleri

Uygulama derlenip test edildikten sonra gerçek arayüz görüntüleri burada paylaşılacaktır.
