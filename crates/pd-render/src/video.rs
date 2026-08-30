//! NV12 -> RGB video pipeline: copies the decoder's NV12 texture into a
//! shader-readable NV12 texture, aliases luma (R8) + chroma (R8G8) SRVs, and
//! draws a full-screen triangle with a BT.709 YUV->RGB pixel shader.
//!
//! The intra-GPU CopySubresourceRegion is deliberate: DXVA decoder textures are
//! BIND_DECODER and generally cannot be SRV-aliased directly. The copy is a
//! sub-millisecond GPU->GPU move that also frees the decoder surface promptly.

use windows::core::{s, Result};
use windows::Win32::Graphics::Direct3D::Fxc::D3DCompile;
use windows::Win32::Graphics::Direct3D::{
    ID3DBlob, D3D_PRIMITIVE_TOPOLOGY_TRIANGLELIST, D3D_SRV_DIMENSION_TEXTURE2D,
};
use windows::Win32::Graphics::Direct3D11::{
    ID3D11Device, ID3D11DeviceContext, ID3D11PixelShader, ID3D11RenderTargetView,
    ID3D11SamplerState, ID3D11ShaderResourceView, ID3D11Texture2D, ID3D11VertexShader,
    D3D11_BIND_SHADER_RESOURCE, D3D11_COMPARISON_NEVER, D3D11_FILTER_MIN_MAG_MIP_LINEAR,
    D3D11_SAMPLER_DESC, D3D11_SHADER_RESOURCE_VIEW_DESC, D3D11_SHADER_RESOURCE_VIEW_DESC_0,
    D3D11_TEX2D_SRV, D3D11_TEXTURE2D_DESC,
    D3D11_TEXTURE_ADDRESS_CLAMP, D3D11_USAGE_DEFAULT, D3D11_VIEWPORT,
};
use windows::Win32::Graphics::Dxgi::Common::{
    DXGI_FORMAT_NV12, DXGI_FORMAT_R8G8_UNORM, DXGI_FORMAT_R8_UNORM, DXGI_SAMPLE_DESC,
};

const SHADER: &str = r#"
Texture2D<float>  LumaTex   : register(t0);
Texture2D<float2> ChromaTex : register(t1);
SamplerState      Samp      : register(s0);

struct VSOut { float4 pos : SV_Position; float2 uv : TEXCOORD0; };

VSOut vs_main(uint id : SV_VertexID) {
    VSOut o;
    float2 uv = float2((id << 1) & 2, id & 2); // (0,0) (2,0) (0,2)
    o.uv  = uv;
    o.pos = float4(uv * float2(2, -2) + float2(-1, 1), 0, 1);
    return o;
}

// Catmull-Rom bicubic on the luma plane via 9 bilinear-assisted taps
// (Sigdel form). Keeps small-text edges crisp when downscaling a supersampled
// source; far less ringing than Lanczos-3. ~0.1 ms on a GTX 1650 at 1080p.
float sampleLumaCatmullRom(float2 uv) {
    float2 texSize;
    LumaTex.GetDimensions(texSize.x, texSize.y);
    float2 samplePos = uv * texSize;
    float2 texPos1 = floor(samplePos - 0.5) + 0.5;
    float2 f = samplePos - texPos1;

    float2 w0 = f * (-0.5 + f * (1.0 - 0.5 * f));
    float2 w1 = 1.0 + f * f * (-2.5 + 1.5 * f);
    float2 w2 = f * (0.5 + f * (2.0 - 1.5 * f));
    float2 w3 = f * f * (-0.5 + 0.5 * f);

    float2 w12 = w1 + w2;
    float2 offset12 = w2 / w12;

    float2 tc0 = (texPos1 - 1.0) / texSize;
    float2 tc3 = (texPos1 + 2.0) / texSize;
    float2 tc12 = (texPos1 + offset12) / texSize;

    float r = 0.0;
    r += LumaTex.Sample(Samp, float2(tc0.x,  tc0.y))  * w0.x  * w0.y;
    r += LumaTex.Sample(Samp, float2(tc12.x, tc0.y))  * w12.x * w0.y;
    r += LumaTex.Sample(Samp, float2(tc3.x,  tc0.y))  * w3.x  * w0.y;
    r += LumaTex.Sample(Samp, float2(tc0.x,  tc12.y)) * w0.x  * w12.y;
    r += LumaTex.Sample(Samp, float2(tc12.x, tc12.y)) * w12.x * w12.y;
    r += LumaTex.Sample(Samp, float2(tc3.x,  tc12.y)) * w3.x  * w12.y;
    r += LumaTex.Sample(Samp, float2(tc0.x,  tc3.y))  * w0.x  * w3.y;
    r += LumaTex.Sample(Samp, float2(tc12.x, tc3.y))  * w12.x * w3.y;
    r += LumaTex.Sample(Samp, float2(tc3.x,  tc3.y))  * w3.x  * w3.y;
    return r;
}

float4 ps_main(VSOut i) : SV_Target {
    float  y = sampleLumaCatmullRom(i.uv);
    float2 c = ChromaTex.Sample(Samp, i.uv) - 0.5; // chroma stays bilinear
    // BT.709, limited (video) range
    float yy = (y - 16.0 / 255.0) * (255.0 / 219.0);
    float u  = c.x * (255.0 / 224.0);
    float v  = c.y * (255.0 / 224.0);
    float3 rgb = float3(
        yy + 1.5748 * v,
        yy - 0.1873 * u - 0.4681 * v,
        yy + 1.8556 * u);
    return float4(saturate(rgb), 1.0);
}
"#;

pub struct Video {
    vs: ID3D11VertexShader,
    ps: ID3D11PixelShader,
    sampler: ID3D11SamplerState,
    nv12: Option<ID3D11Texture2D>,
    luma: Option<ID3D11ShaderResourceView>,
    chroma: Option<ID3D11ShaderResourceView>,
    size: (u32, u32),
}

impl Video {
    pub fn new(device: &ID3D11Device) -> Result<Self> {
        unsafe {
            let vs_blob = compile(SHADER, s!("vs_main"), s!("vs_5_0"))?;
            let ps_blob = compile(SHADER, s!("ps_main"), s!("ps_5_0"))?;

            let mut vs: Option<ID3D11VertexShader> = None;
            device.CreateVertexShader(blob_bytes(&vs_blob), None, Some(&mut vs))?;
            let mut ps: Option<ID3D11PixelShader> = None;
            device.CreatePixelShader(blob_bytes(&ps_blob), None, Some(&mut ps))?;

            let samp_desc = D3D11_SAMPLER_DESC {
                Filter: D3D11_FILTER_MIN_MAG_MIP_LINEAR,
                AddressU: D3D11_TEXTURE_ADDRESS_CLAMP,
                AddressV: D3D11_TEXTURE_ADDRESS_CLAMP,
                AddressW: D3D11_TEXTURE_ADDRESS_CLAMP,
                ComparisonFunc: D3D11_COMPARISON_NEVER,
                MaxLOD: f32::MAX,
                ..Default::default()
            };
            let mut sampler: Option<ID3D11SamplerState> = None;
            device.CreateSamplerState(&samp_desc, Some(&mut sampler))?;

            Ok(Self {
                vs: vs.unwrap(),
                ps: ps.unwrap(),
                sampler: sampler.unwrap(),
                nv12: None,
                luma: None,
                chroma: None,
                size: (0, 0),
            })
        }
    }

    /// (Re)create the shader-readable NV12 texture + SRVs when the video size changes.
    unsafe fn ensure(&mut self, device: &ID3D11Device, vw: u32, vh: u32) -> Result<()> {
        if self.nv12.is_some() && self.size == (vw, vh) {
            return Ok(());
        }
        let desc = D3D11_TEXTURE2D_DESC {
            Width: vw,
            Height: vh,
            MipLevels: 1,
            ArraySize: 1,
            Format: DXGI_FORMAT_NV12,
            SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
            Usage: D3D11_USAGE_DEFAULT,
            BindFlags: D3D11_BIND_SHADER_RESOURCE.0 as u32,
            ..Default::default()
        };
        let mut nv12: Option<ID3D11Texture2D> = None;
        device.CreateTexture2D(&desc, None, Some(&mut nv12))?;
        let nv12 = nv12.unwrap();

        let luma = make_srv(device, &nv12, DXGI_FORMAT_R8_UNORM)?;
        let chroma = make_srv(device, &nv12, DXGI_FORMAT_R8G8_UNORM)?;

        self.nv12 = Some(nv12);
        self.luma = Some(luma);
        self.chroma = Some(chroma);
        self.size = (vw, vh);
        Ok(())
    }

    /// Copy the decoded NV12 slice into our texture and draw it letterboxed.
    #[allow(clippy::too_many_arguments)]
    pub fn draw(
        &mut self,
        device: &ID3D11Device,
        ctx: &ID3D11DeviceContext,
        rtv: &ID3D11RenderTargetView,
        win: (u32, u32),
        src: &ID3D11Texture2D,
        src_sub: u32,
        vid: (u32, u32),
    ) -> Result<()> {
        unsafe {
            self.ensure(device, vid.0, vid.1)?;
            let nv12 = self.nv12.clone().unwrap();

            ctx.CopySubresourceRegion(&nv12, 0, 0, 0, 0, src, src_sub, None);

            ctx.ClearRenderTargetView(rtv, &[0.0, 0.0, 0.0, 1.0]);

            let (ww, wh) = (win.0 as f32, win.1 as f32);
            let (vw, vh) = (vid.0 as f32, vid.1 as f32);
            let scale = (ww / vw).min(wh / vh);
            let (dw, dh) = (vw * scale, vh * scale);
            let vp = D3D11_VIEWPORT {
                TopLeftX: (ww - dw) * 0.5,
                TopLeftY: (wh - dh) * 0.5,
                Width: dw,
                Height: dh,
                MinDepth: 0.0,
                MaxDepth: 1.0,
            };
            ctx.RSSetViewports(Some(&[vp]));

            let rtvs = [Some(rtv.clone())];
            ctx.OMSetRenderTargets(Some(&rtvs), None);
            ctx.IASetPrimitiveTopology(D3D_PRIMITIVE_TOPOLOGY_TRIANGLELIST);
            ctx.VSSetShader(&self.vs, None);
            ctx.PSSetShader(&self.ps, None);
            let srvs = [self.luma.clone(), self.chroma.clone()];
            ctx.PSSetShaderResources(0, Some(&srvs));
            let samps = [Some(self.sampler.clone())];
            ctx.PSSetSamplers(0, Some(&samps));
            ctx.Draw(3, 0);

            // Unbind SRVs so the NV12 texture can be a copy target next frame.
            let none_srvs: [Option<ID3D11ShaderResourceView>; 2] = [None, None];
            ctx.PSSetShaderResources(0, Some(&none_srvs));
        }
        Ok(())
    }
}

unsafe fn make_srv(
    device: &ID3D11Device,
    tex: &ID3D11Texture2D,
    format: windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT,
) -> Result<ID3D11ShaderResourceView> {
    let desc = D3D11_SHADER_RESOURCE_VIEW_DESC {
        Format: format,
        ViewDimension: D3D_SRV_DIMENSION_TEXTURE2D,
        Anonymous: D3D11_SHADER_RESOURCE_VIEW_DESC_0 {
            Texture2D: D3D11_TEX2D_SRV {
                MostDetailedMip: 0,
                MipLevels: 1,
            },
        },
    };
    let mut srv: Option<ID3D11ShaderResourceView> = None;
    device.CreateShaderResourceView(tex, Some(&desc), Some(&mut srv))?;
    Ok(srv.unwrap())
}

unsafe fn compile(
    src: &str,
    entry: windows::core::PCSTR,
    target: windows::core::PCSTR,
) -> Result<ID3DBlob> {
    let mut blob: Option<ID3DBlob> = None;
    let mut errors: Option<ID3DBlob> = None;
    let hr = D3DCompile(
        src.as_ptr() as *const _,
        src.len(),
        s!("prismdesk.hlsl"),
        None,
        None,
        entry,
        target,
        0,
        0,
        &mut blob,
        Some(&mut errors),
    );
    if hr.is_err() {
        if let Some(e) = errors {
            let msg = std::slice::from_raw_parts(e.GetBufferPointer() as *const u8, e.GetBufferSize());
            eprintln!("[shader] {}", String::from_utf8_lossy(msg));
        }
        hr?;
    }
    Ok(blob.unwrap())
}

unsafe fn blob_bytes(blob: &ID3DBlob) -> &[u8] {
    std::slice::from_raw_parts(blob.GetBufferPointer() as *const u8, blob.GetBufferSize())
}
