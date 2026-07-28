# NVIDIA Optical Flow API headers

These API-only headers are pinned to `jp7677/dxvk-nvapi` commit
`ffb351d627801570c1e39570d0e3f0033a494322`:

- `nvOpticalFlowCommon.h`: `f133879f7add54d42d71f5d7839a05962af7aaa3620909e3cbfbece77119d30f`
- `nvOpticalFlowD3D11.h`: `4721a36321591e6882e68ad342f5113a64e6c55f066b563e205fd9cc1a91cae3`

The files carry NVIDIA's MIT license notice. MediaStationGo dynamically loads
the Optical Flow implementation from the NVIDIA display driver's
`nvofapi64.dll`; no Optical Flow SDK binary is bundled here.

