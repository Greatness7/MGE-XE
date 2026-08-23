use crate::abi::{D3dxPlane, ViewFrustum};

pub(crate) fn test_frustum_with_extent(extent: f32) -> ViewFrustum {
    ViewFrustum {
        frustum: [
            D3dxPlane {
                a: 1.0,
                b: 0.0,
                c: 0.0,
                d: extent,
            },
            D3dxPlane {
                a: -1.0,
                b: 0.0,
                c: 0.0,
                d: extent,
            },
            D3dxPlane {
                a: 0.0,
                b: 1.0,
                c: 0.0,
                d: extent,
            },
            D3dxPlane {
                a: 0.0,
                b: -1.0,
                c: 0.0,
                d: extent,
            },
            D3dxPlane {
                a: 0.0,
                b: 0.0,
                c: 1.0,
                d: extent,
            },
            D3dxPlane {
                a: 0.0,
                b: 0.0,
                c: -1.0,
                d: extent,
            },
        ],
    }
}

pub(crate) fn test_frustum() -> ViewFrustum {
    test_frustum_with_extent(1000.0)
}
