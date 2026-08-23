use bytemuck::{Pod, Zeroable};
use glam::Vec3;

/// ABI-compatible 2D vector used in the D3DX-style math layer.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, Zeroable, Pod)]
pub struct D3dxVector2 {
    pub x: f32,
    pub y: f32,
}

/// ABI-compatible 3D vector used in the D3DX-style math layer.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, Zeroable, Pod)]
pub struct D3dxVector3 {
    pub x: f32,
    pub y: f32,
    pub z: f32,
}

/// ABI-compatible 4D vector used for eye position plus radius queries.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, Zeroable, Pod)]
pub struct D3dxVector4 {
    pub x: f32,
    pub y: f32,
    pub z: f32,
    pub w: f32,
}

/// Plane equation in `ax + by + cz + d = 0` form.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, Zeroable, Pod)]
pub struct D3dxPlane {
    pub a: f32,
    pub b: f32,
    pub c: f32,
    pub d: f32,
}

/// Row-major 4x4 matrix matching the original D3DX layout used by MGE XE.
#[repr(C)]
#[derive(Clone, Copy, Debug, Zeroable, Pod)]
pub struct D3dxMatrix {
    pub _11: f32,
    pub _12: f32,
    pub _13: f32,
    pub _14: f32,
    pub _21: f32,
    pub _22: f32,
    pub _23: f32,
    pub _24: f32,
    pub _31: f32,
    pub _32: f32,
    pub _33: f32,
    pub _34: f32,
    pub _41: f32,
    pub _42: f32,
    pub _43: f32,
    pub _44: f32,
}

impl Default for D3dxMatrix {
    fn default() -> Self {
        Self::identity()
    }
}

impl D3dxMatrix {
    /// Returns the identity matrix.
    #[rustfmt::skip]
    pub const fn identity() -> Self {
        Self {
            _11: 1.0, _12: 0.0, _13: 0.0, _14: 0.0,
            _21: 0.0, _22: 1.0, _23: 0.0, _24: 0.0,
            _31: 0.0, _32: 0.0, _33: 1.0, _34: 0.0,
            _41: 0.0, _42: 0.0, _43: 0.0, _44: 1.0,
        }
    }

    /// Returns a matrix that translates row vectors by the given offset.
    pub fn translation(x: f32, y: f32, z: f32) -> Self {
        let mut value = Self::identity();
        value._41 = x;
        value._42 = y;
        value._43 = z;
        value
    }

    /// Returns a matrix that scales row vectors along each axis.
    #[rustfmt::skip]
    pub fn scaling(x: f32, y: f32, z: f32) -> Self {
        Self {
            _11: x,   _12: 0.0, _13: 0.0, _14: 0.0,
            _21: 0.0, _22: y,   _23: 0.0, _24: 0.0,
            _31: 0.0, _32: 0.0, _33: z,   _34: 0.0,
            _41: 0.0, _42: 0.0, _43: 0.0, _44: 1.0,
        }
    }

    /// Returns a row-vector rotation matrix around the X axis.
    #[rustfmt::skip]
    pub fn rotation_x(angle: f32) -> Self {
        let (sin, cos) = angle.sin_cos();
        Self {
            _11: 1.0, _12: 0.0,  _13: 0.0, _14: 0.0,
            _21: 0.0, _22: cos,  _23: sin, _24: 0.0,
            _31: 0.0, _32: -sin, _33: cos, _34: 0.0,
            _41: 0.0, _42: 0.0,  _43: 0.0, _44: 1.0,
        }
    }

    /// Returns a row-vector rotation matrix around the Y axis.
    #[rustfmt::skip]
    pub fn rotation_y(angle: f32) -> Self {
        let (sin, cos) = angle.sin_cos();
        Self {
            _11: cos, _12: 0.0, _13: -sin, _14: 0.0,
            _21: 0.0, _22: 1.0, _23: 0.0,  _24: 0.0,
            _31: sin, _32: 0.0, _33: cos,  _34: 0.0,
            _41: 0.0, _42: 0.0, _43: 0.0,  _44: 1.0,
        }
    }

    /// Returns a row-vector rotation matrix around the Z axis.
    #[rustfmt::skip]
    pub fn rotation_z(angle: f32) -> Self {
        let (sin, cos) = angle.sin_cos();
        Self {
            _11: cos,  _12: sin, _13: 0.0, _14: 0.0,
            _21: -sin, _22: cos, _23: 0.0, _24: 0.0,
            _31: 0.0,  _32: 0.0, _33: 1.0, _34: 0.0,
            _41: 0.0,  _42: 0.0, _43: 0.0, _44: 1.0,
        }
    }

    /// Multiplies two matrices using D3DX row-vector semantics.
    ///
    /// The resulting matrix applies `self` first and `rhs` second, matching the
    /// way the original MGE code composes transforms.
    #[rustfmt::skip]
    pub fn multiply(self, rhs: Self) -> Self {
        let left = self.as_rows();
        let right = rhs.as_rows();
        let mut out = [[0.0; 4]; 4];
        let mut row = 0;
        while row < 4 {
            let mut col = 0;
            while col < 4 {
                out[row][col] = left[row][0] * right[0][col]
                              + left[row][1] * right[1][col]
                              + left[row][2] * right[2][col]
                              + left[row][3] * right[3][col];
                col += 1;
            }
            row += 1;
        }
        Self::from_rows(out)
    }

    /// Transforms a position by the matrix, dividing by `w` when needed.
    pub fn transform_coord(self, value: D3dxVector3) -> D3dxVector3 {
        let x = value.x * self._11 + value.y * self._21 + value.z * self._31 + self._41;
        let y = value.x * self._12 + value.y * self._22 + value.z * self._32 + self._42;
        let z = value.x * self._13 + value.y * self._23 + value.z * self._33 + self._43;
        let w = value.x * self._14 + value.y * self._24 + value.z * self._34 + self._44;
        if w != 0.0 {
            D3dxVector3 {
                x: x / w,
                y: y / w,
                z: z / w,
            }
        } else {
            D3dxVector3 { x, y, z }
        }
    }

    /// Transforms a direction vector, ignoring translation and homogeneous divide.
    pub fn transform_normal(self, value: D3dxVector3) -> D3dxVector3 {
        D3dxVector3 {
            x: value.x * self._11 + value.y * self._21 + value.z * self._31,
            y: value.x * self._12 + value.y * self._22 + value.z * self._32,
            z: value.x * self._13 + value.y * self._23 + value.z * self._33,
        }
    }

    fn as_rows(self) -> [[f32; 4]; 4] {
        [
            [self._11, self._12, self._13, self._14],
            [self._21, self._22, self._23, self._24],
            [self._31, self._32, self._33, self._34],
            [self._41, self._42, self._43, self._44],
        ]
    }

    fn from_rows(rows: [[f32; 4]; 4]) -> Self {
        Self {
            _11: rows[0][0],
            _12: rows[0][1],
            _13: rows[0][2],
            _14: rows[0][3],
            _21: rows[1][0],
            _22: rows[1][1],
            _23: rows[1][2],
            _24: rows[1][3],
            _31: rows[2][0],
            _32: rows[2][1],
            _33: rows[2][2],
            _34: rows[2][3],
            _41: rows[3][0],
            _42: rows[3][1],
            _43: rows[3][2],
            _44: rows[3][3],
        }
    }
}

/// Bounding sphere used for coarse visibility tests.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, Zeroable, Pod)]
pub struct BoundingSphere {
    pub center: D3dxVector3,
    pub radius: f32,
}

impl BoundingSphere {
    /// Returns `true` when the sphere has no extent.
    pub fn empty(self) -> bool {
        self.radius == 0.0
    }

    /// Returns a sphere that encloses `self` and `rhs`.
    pub fn union_with(self, rhs: Self) -> Self {
        let mut result = self;
        let vector = rhs.center - self.center;
        let distance = vector.length();

        // Treat nearly coincident centers and empty spheres as degenerate input so we keep
        // the larger sphere instead of amplifying floating-point noise.
        if distance <= 0.001 || self.radius == 0.0 || rhs.radius == 0.0 {
            if rhs.radius > self.radius {
                result.center = rhs.center;
                result.radius = rhs.radius;
            }
            return result;
        }

        if -self.radius < distance - rhs.radius && self.radius > distance + rhs.radius {
            return result;
        }

        if distance - rhs.radius < -self.radius && distance + rhs.radius > self.radius {
            return rhs;
        }

        let direction = vector.normalize();
        result.radius = 0.5 * (distance + self.radius + rhs.radius);
        let coefficient = 0.5 * (distance + rhs.radius - self.radius);
        result.center = self.center + direction * coefficient;
        result
    }
}

/// Oriented bounding box represented by a center and three basis extent vectors.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, Zeroable, Pod)]
pub struct BoundingBox {
    pub center: D3dxVector3,
    pub vx: D3dxVector3,
    pub vy: D3dxVector3,
    pub vz: D3dxVector3,
}

impl BoundingBox {
    /// Initializes the box from axis-aligned minimum and maximum corners.
    pub fn set(&mut self, min: D3dxVector3, max: D3dxVector3) {
        self.center = (min + max) * 0.5;
        self.vx = D3dxVector3 {
            x: 0.5 * (max.x - min.x),
            y: 0.0,
            z: 0.0,
        };
        self.vy = D3dxVector3 {
            x: 0.0,
            y: 0.5 * (max.y - min.y),
            z: 0.0,
        };
        self.vz = D3dxVector3 {
            x: 0.0,
            y: 0.0,
            z: 0.5 * (max.z - min.z),
        };
    }

    /// Applies a transform to the box center and basis extents.
    pub fn transform(&mut self, matrix: D3dxMatrix) {
        self.center = matrix.transform_coord(self.center);
        self.vx = matrix.transform_normal(self.vx);
        self.vy = matrix.transform_normal(self.vy);
        self.vz = matrix.transform_normal(self.vz);
    }
}

/// Six-plane view frustum used for visibility tests.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, Zeroable, Pod)]
pub struct ViewFrustum {
    /// Frustum planes in the order expected by the C++ runtime.
    pub frustum: [D3dxPlane; 6],
}

/// Relation between a primitive and a containing volume.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Containment {
    Inside,
    Outside,
    Intersects,
}

impl ViewFrustum {
    /// Tests a bounding sphere against the frustum.
    pub fn contains_sphere(&self, sphere: &BoundingSphere) -> Containment {
        let mut distances = [0.0_f32; 6];
        for (index, plane) in self.frustum.iter().enumerate() {
            let distance = plane.dot_coord(sphere.center);
            distances[index] = distance;
            if distance + sphere.radius < 0.0 {
                return Containment::Outside;
            }
        }
        for distance in distances {
            if distance.abs() < sphere.radius {
                return Containment::Intersects;
            }
        }
        Containment::Inside
    }

    /// Tests an oriented bounding box against the frustum.
    ///
    /// Returns `Outside` when the box is fully outside; otherwise returns `Inside` (the test is conservative and never returns `Intersects`).
    pub fn contains_box(&self, box_value: &BoundingBox) -> Containment {
        for plane in self.frustum {
            let extent_x = plane.dot_normal(box_value.vx).abs();
            let extent_y = plane.dot_normal(box_value.vy).abs();
            let extent_z = plane.dot_normal(box_value.vz).abs();
            let distance = plane.dot_coord(box_value.center);
            if distance + extent_x + extent_y + extent_z < 0.0 {
                return Containment::Outside;
            }
        }
        Containment::Inside
    }
}

impl D3dxPlane {
    /// Evaluates the plane equation at a point.
    pub fn dot_coord(self, value: D3dxVector3) -> f32 {
        self.a * value.x + self.b * value.y + self.c * value.z + self.d
    }

    /// Evaluates the plane normal against a direction vector.
    pub fn dot_normal(self, value: D3dxVector3) -> f32 {
        self.a * value.x + self.b * value.y + self.c * value.z
    }
}

impl std::ops::Add for D3dxVector3 {
    type Output = Self;

    fn add(self, rhs: Self) -> Self::Output {
        Self {
            x: self.x + rhs.x,
            y: self.y + rhs.y,
            z: self.z + rhs.z,
        }
    }
}

impl std::ops::Sub for D3dxVector3 {
    type Output = Self;

    fn sub(self, rhs: Self) -> Self::Output {
        Self {
            x: self.x - rhs.x,
            y: self.y - rhs.y,
            z: self.z - rhs.z,
        }
    }
}

impl std::ops::Mul<f32> for D3dxVector3 {
    type Output = Self;

    fn mul(self, rhs: f32) -> Self::Output {
        Self {
            x: self.x * rhs,
            y: self.y * rhs,
            z: self.z * rhs,
        }
    }
}

impl D3dxVector3 {
    /// Returns the Euclidean length of the vector.
    pub fn length(self) -> f32 {
        Vec3::new(self.x, self.y, self.z).length()
    }

    /// Returns a normalized vector, or zero when the input has no length.
    pub fn normalize(self) -> Self {
        let value = Vec3::new(self.x, self.y, self.z).normalize_or_zero();
        Self {
            x: value.x,
            y: value.y,
            z: value.z,
        }
    }
}

impl std::ops::Add for D3dxVector2 {
    type Output = Self;

    fn add(self, rhs: Self) -> Self::Output {
        Self {
            x: self.x + rhs.x,
            y: self.y + rhs.y,
        }
    }
}

impl std::ops::Mul<f32> for D3dxVector2 {
    type Output = Self;

    fn mul(self, rhs: f32) -> Self::Output {
        Self {
            x: self.x * rhs,
            y: self.y * rhs,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::f32::consts::FRAC_PI_2;

    fn assert_near(actual: f32, expected: f32) {
        assert!((actual - expected).abs() < 0.00001, "{actual} != {expected}");
    }

    #[test]
    fn matrix_transform_matches_d3dx_row_vector_convention() {
        let transform = D3dxMatrix::scaling(2.0, 2.0, 2.0).multiply(D3dxMatrix::translation(10.0, 20.0, 30.0));
        let point = transform.transform_coord(D3dxVector3 { x: 1.0, y: 2.0, z: 3.0 });
        assert_eq!(point.x, 12.0);
        assert_eq!(point.y, 24.0);
        assert_eq!(point.z, 36.0);
    }

    #[test]
    fn rotation_matrices_match_d3dx_axis_conventions() {
        let rotated_x = D3dxMatrix::rotation_x(FRAC_PI_2).transform_normal(D3dxVector3 { x: 0.0, y: 1.0, z: 0.0 });
        assert_near(rotated_x.x, 0.0);
        assert_near(rotated_x.y, 0.0);
        assert_near(rotated_x.z, 1.0);

        let rotated_y = D3dxMatrix::rotation_y(FRAC_PI_2).transform_normal(D3dxVector3 { x: 1.0, y: 0.0, z: 0.0 });
        assert_near(rotated_y.x, 0.0);
        assert_near(rotated_y.y, 0.0);
        assert_near(rotated_y.z, -1.0);

        let rotated_z = D3dxMatrix::rotation_z(FRAC_PI_2).transform_normal(D3dxVector3 { x: 1.0, y: 0.0, z: 0.0 });
        assert_near(rotated_z.x, 0.0);
        assert_near(rotated_z.y, 1.0);
        assert_near(rotated_z.z, 0.0);
    }

    #[test]
    fn matrix_multiply_applies_left_transform_before_right_transform() {
        let transform = D3dxMatrix::translation(10.0, 20.0, 30.0).multiply(D3dxMatrix::scaling(2.0, 2.0, 2.0));
        let point = transform.transform_coord(D3dxVector3 { x: 1.0, y: 2.0, z: 3.0 });
        assert_eq!(point.x, 22.0);
        assert_eq!(point.y, 44.0);
        assert_eq!(point.z, 66.0);
    }
}
