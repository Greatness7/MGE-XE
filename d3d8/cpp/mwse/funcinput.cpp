
#include "funcinput.h"
#include "mge/mgedinput.h"



MWSEINSTRUCTION_DECLARE_VTABLE(mwseTapKey)

bool mwseTapKey::execute(mwseInstruction* _this) {
    VMREGTYPE key;
    if (!_this->vmPop(&key)) { return false; }

    MGEProxyDirectInput::changeKeyBehavior(key, MGEProxyDirectInput::TAP, true);
    return true;
}

MWSEINSTRUCTION_DECLARE_VTABLE(mwsePushKey)

bool mwsePushKey::execute(mwseInstruction* _this) {
    VMREGTYPE key;
    if (!_this->vmPop(&key)) { return false; }

    MGEProxyDirectInput::changeKeyBehavior(key, MGEProxyDirectInput::PUSH, true);
    return true;
}

MWSEINSTRUCTION_DECLARE_VTABLE(mwseReleaseKey)

bool mwseReleaseKey::execute(mwseInstruction* _this) {
    VMREGTYPE key;
    if (!_this->vmPop(&key)) { return false; }

    MGEProxyDirectInput::changeKeyBehavior(key, MGEProxyDirectInput::PUSH, false);
    return true;
}

MWSEINSTRUCTION_DECLARE_VTABLE(mwseHammerKey)

bool mwseHammerKey::execute(mwseInstruction* _this) {
    VMREGTYPE key;
    if (!_this->vmPop(&key)) { return false; }

    MGEProxyDirectInput::changeKeyBehavior(key, MGEProxyDirectInput::HAMMER, true);
    return true;
}

MWSEINSTRUCTION_DECLARE_VTABLE(mwseUnhammerKey)

bool mwseUnhammerKey::execute(mwseInstruction* _this) {
    VMREGTYPE key;
    if (!_this->vmPop(&key)) { return false; }

    MGEProxyDirectInput::changeKeyBehavior(key, MGEProxyDirectInput::HAMMER, false);
    return true;
}

MWSEINSTRUCTION_DECLARE_VTABLE(mwseAHammerKey)

bool mwseAHammerKey::execute(mwseInstruction* _this) {
    VMREGTYPE key;
    if (!_this->vmPop(&key)) { return false; }

    MGEProxyDirectInput::changeKeyBehavior(key, MGEProxyDirectInput::AHAMMER, true);
    return true;
}

MWSEINSTRUCTION_DECLARE_VTABLE(mwseAUnhammerKey)

bool mwseAUnhammerKey::execute(mwseInstruction* _this) {
    VMREGTYPE key;
    if (!_this->vmPop(&key)) { return false; }

    MGEProxyDirectInput::changeKeyBehavior(key, MGEProxyDirectInput::AHAMMER, false);
    return true;
}

MWSEINSTRUCTION_DECLARE_VTABLE(mwseDisallowKey)

bool mwseDisallowKey::execute(mwseInstruction* _this) {
    VMREGTYPE key;
    if (!_this->vmPop(&key)) { return false; }

    MGEProxyDirectInput::changeKeyBehavior(key, MGEProxyDirectInput::DISALLOW, true);
    return true;
}

MWSEINSTRUCTION_DECLARE_VTABLE(mwseAllowKey)

bool mwseAllowKey::execute(mwseInstruction* _this) {
    VMREGTYPE key;
    if (!_this->vmPop(&key)) { return false; }

    MGEProxyDirectInput::changeKeyBehavior(key, MGEProxyDirectInput::DISALLOW, false);
    return true;
}
