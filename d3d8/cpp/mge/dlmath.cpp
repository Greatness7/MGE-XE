
#include "dlmath.h"

#include <cmath>

BoundingSphere::BoundingSphere() : center(0.0f, 0.0f, 0.0f), radius(0.0f) {
}

BoundingSphere::BoundingSphere(const BoundingSphere& sphere) {
    *this = sphere;
}

BoundingSphere& BoundingSphere::operator=(const BoundingSphere& sphere) {
    center.x = sphere.center.x;
    center.y = sphere.center.y;
    center.z = sphere.center.z;
    radius = sphere.radius;

    return *this;
}

BoundingSphere& BoundingSphere::operator+=(const BoundingSphere& rh) {
    // Compute the minimum bounding sphere containing both inputs. The formula follows XEngine's
    // sphere merge, with its coefficient subtraction corrected to (dist + r1 - r0) / 2.
    BoundingSphere result;
    D3DXVECTOR3 vect = rh.center - center;
    float dist = D3DXVec3Length(&vect);

    // Coincident centers or a zero-radius input reduce to the larger sphere.
    if (dist <= 0.001f || radius == 0.0f || rh.radius == 0.0f) {
        if (rh.radius > radius) {
            radius = rh.radius;
            center = rh.center;
        }
        return *this;
    }

    // Interval for first sphere is [-r0, r0]; interval for second sphere is [d - r1, d + r1]
    if (-radius < dist - rh.radius && radius > dist + rh.radius) {
        // The right-hand sphere is contained in this one.
        result.center = center;
        result.radius = radius;
    } else if (dist - rh.radius < -radius && dist + rh.radius > radius) {
        // This sphere is contained in the right-hand one.
        result.center = rh.center;
        result.radius = rh.radius;
    } else {
        // For overlapping spheres, radius = (dist + r0 + r1) / 2 and the center lies on the
        // center line at coef = (dist + r1 - r0) / 2 from the first sphere.
        D3DXVECTOR3 norm_vect;
        result.radius = 0.5f * (dist + radius + rh.radius);
        D3DXVec3Normalize(&norm_vect, &vect);

        float coef = 0.5f * (dist + rh.radius - radius);
        result.center = center + coef * norm_vect;
    }

    center = result.center;
    radius = result.radius;

    return *this;
}

BoundingSphere BoundingSphere::operator+(const BoundingSphere& rh) {
    return BoundingSphere(*this) += rh;
}

BoundingBox::BoundingBox() {
    center = D3DXVECTOR3(0.0, 0.0, 0.0);
    vx = vy = vz = center;
}

BoundingBox::BoundingBox(const BoundingBox& rh) {
    *this = rh;
}

BoundingBox& BoundingBox::operator=(const BoundingBox& rh) {
    center = rh.center;
    vx = rh.vx;
    vy = rh.vy;
    vz = rh.vz;

    return *this;
}

BoundingBox::BoundingBox(const D3DXVECTOR3& min, const D3DXVECTOR3& max) {
    Set(min, max);
}

void BoundingBox::Set(const D3DXVECTOR3& min, const D3DXVECTOR3& max) {
    center = 0.5f * (min + max);
    vx = D3DXVECTOR3(0.5f * (max.x - min.x), 0, 0);
    vy = D3DXVECTOR3(0, 0.5f * (max.y - min.y), 0);
    vz = D3DXVECTOR3(0, 0, 0.5f * (max.z - min.z));
}

void BoundingBox::Transform(const D3DXMATRIX& m) {
    D3DXVec3TransformCoord(&center, &center, &m);
    D3DXVec3TransformNormal(&vx, &vx, &m);
    D3DXVec3TransformNormal(&vy, &vy, &m);
    D3DXVec3TransformNormal(&vz, &vz, &m);
}