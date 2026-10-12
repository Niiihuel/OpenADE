# Durable user images over Responses

`ModelBackend::supports_user_images()` reports transport support; the host must also check the selected model/provider's image capability before offering image submission. Messages and Responses send images when the model reads them. Chat Completions sends them as `image_url` parts when its backend was built with the model's image capability, and otherwise returns an explicit `InvalidRequest` if an unadapted message contains images. Images returned by tools are covered in [native-tool-images.md](native-tool-images.md).

Core stores each image inline in `AgentMessage.images` as verified Base64 with a MIME type, size, and SHA-256 digest. Responses rebuilds a data URL with `UserImage::verified_data_url()` on every request, including requests reconstructed after native resume. It does not send local paths or fetch remote URLs.

For a user message with images, the wire `content` array contains an `input_text` part when text is present, followed by `input_image` parts in the stored image order. Text-only messages retain the existing string form. The same mapping is used for streaming and nonstreaming requests. Text-only request profile fingerprints remain compatible with prior journals. Core's durable image record and digest bind the per-turn image bytes; the fingerprint represents backend options rather than per-turn content.

The [official OpenAI image-input guide](https://developers.openai.com/api/docs/guides/images-vision) documents the Responses `input_image` shape and Base64 data URLs. Local HTTP tests verify the actual streaming request, resumed-message serialization round trip, live text deltas, corrupt-image rejection before HTTP, and the Chat Completions rejection path for a backend without the image capability.

Transport support does not guarantee a model accepts every image format or size. The backend reports provider errors explicitly. Core enforces PNG/JPEG/WEBP/GIF signatures, 8 MiB per image, eight images and 24 MiB per message; see `docs/native-user-images.md` for durable storage and replay rules.
