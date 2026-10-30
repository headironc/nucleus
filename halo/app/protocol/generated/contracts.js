// Generated from protocol/openapi.json. Do not edit.
export default {
  "POST /api/v1/codes": {
    "200": {
      "validator": "sendCode_200",
      "codes": [
        "ok"
      ]
    },
    "400": {
      "validator": "sendCode_400",
      "codes": [
        "invalid_input"
      ]
    },
    "401": {
      "validator": "sendCode_401",
      "codes": [
        "authentication_invalid"
      ]
    },
    "403": {
      "validator": "sendCode_403",
      "codes": [
        "verification_invalid",
        "permission_denied"
      ]
    },
    "409": {
      "validator": "sendCode_409",
      "codes": [
        "request_mismatch"
      ]
    },
    "410": {
      "validator": "sendCode_410",
      "codes": [
        "verification_required"
      ]
    },
    "413": {
      "validator": "sendCode_413",
      "codes": [
        "invalid_input"
      ]
    },
    "415": {
      "validator": "sendCode_415",
      "codes": [
        "invalid_input"
      ]
    },
    "422": {
      "validator": "sendCode_422",
      "codes": [
        "invalid_input"
      ]
    },
    "429": {
      "validator": "sendCode_429",
      "codes": [
        "rate_limited"
      ]
    },
    "500": {
      "validator": "sendCode_500",
      "codes": [
        "internal_error"
      ]
    },
    "503": {
      "validator": "sendCode_503",
      "codes": [
        "dependency_unavailable",
        "outcome_unknown"
      ]
    }
  },
  "GET /api/v1/settings": {
    "200": {
      "validator": "readPublicSettings_200",
      "codes": [
        "ok"
      ]
    },
    "400": {
      "validator": "readPublicSettings_400",
      "codes": [
        "invalid_input"
      ]
    }
  },
  "POST /api/v1/sessions": {
    "200": {
      "validator": "createSession_200",
      "codes": [
        "ok"
      ]
    },
    "400": {
      "validator": "createSession_400",
      "codes": [
        "invalid_input"
      ]
    },
    "401": {
      "validator": "createSession_401",
      "codes": [
        "authentication_invalid"
      ]
    },
    "403": {
      "validator": "createSession_403",
      "codes": [
        "verification_invalid",
        "permission_denied"
      ]
    },
    "409": {
      "validator": "createSession_409",
      "codes": [
        "request_mismatch"
      ]
    },
    "410": {
      "validator": "createSession_410",
      "codes": [
        "verification_required"
      ]
    },
    "413": {
      "validator": "createSession_413",
      "codes": [
        "invalid_input"
      ]
    },
    "415": {
      "validator": "createSession_415",
      "codes": [
        "invalid_input"
      ]
    },
    "422": {
      "validator": "createSession_422",
      "codes": [
        "invalid_input"
      ]
    },
    "429": {
      "validator": "createSession_429",
      "codes": [
        "rate_limited"
      ]
    },
    "500": {
      "validator": "createSession_500",
      "codes": [
        "internal_error"
      ]
    },
    "503": {
      "validator": "createSession_503",
      "codes": [
        "dependency_unavailable",
        "outcome_unknown"
      ]
    }
  },
  "DELETE /api/v1/sessions": {
    "200": {
      "validator": "revokeAllSessions_200",
      "codes": [
        "ok"
      ]
    },
    "400": {
      "validator": "revokeAllSessions_400",
      "codes": [
        "invalid_input"
      ]
    },
    "401": {
      "validator": "revokeAllSessions_401",
      "codes": [
        "authentication_invalid"
      ]
    },
    "403": {
      "validator": "revokeAllSessions_403",
      "codes": [
        "permission_denied"
      ]
    },
    "409": {
      "validator": "revokeAllSessions_409",
      "codes": [
        "request_mismatch"
      ]
    },
    "413": {
      "validator": "revokeAllSessions_413",
      "codes": [
        "invalid_input"
      ]
    },
    "429": {
      "validator": "revokeAllSessions_429",
      "codes": [
        "rate_limited"
      ]
    },
    "500": {
      "validator": "revokeAllSessions_500",
      "codes": [
        "internal_error"
      ]
    },
    "503": {
      "validator": "revokeAllSessions_503",
      "codes": [
        "dependency_unavailable",
        "outcome_unknown"
      ]
    }
  },
  "POST /api/v1/registrations": {
    "200": {
      "validator": "register_200",
      "codes": [
        "ok"
      ]
    },
    "201": {
      "validator": "register_201",
      "codes": [
        "ok"
      ]
    },
    "400": {
      "validator": "register_400",
      "codes": [
        "invalid_input"
      ]
    },
    "401": {
      "validator": "register_401",
      "codes": [
        "authentication_invalid"
      ]
    },
    "403": {
      "validator": "register_403",
      "codes": [
        "permission_denied"
      ]
    },
    "409": {
      "validator": "register_409",
      "codes": [
        "request_mismatch",
        "mobile_taken",
        "id_number_taken"
      ]
    },
    "410": {
      "validator": "register_410",
      "codes": [
        "verification_required"
      ]
    },
    "413": {
      "validator": "register_413",
      "codes": [
        "invalid_input"
      ]
    },
    "415": {
      "validator": "register_415",
      "codes": [
        "invalid_input"
      ]
    },
    "422": {
      "validator": "register_422",
      "codes": [
        "invalid_input"
      ]
    },
    "429": {
      "validator": "register_429",
      "codes": [
        "rate_limited"
      ]
    },
    "500": {
      "validator": "register_500",
      "codes": [
        "internal_error"
      ]
    },
    "503": {
      "validator": "register_503",
      "codes": [
        "dependency_unavailable",
        "outcome_unknown"
      ]
    }
  },
  "GET /api/v1/accounts/current": {
    "200": {
      "validator": "currentAccount_200",
      "codes": [
        "ok"
      ]
    },
    "400": {
      "validator": "currentAccount_400",
      "codes": [
        "invalid_input"
      ]
    },
    "401": {
      "validator": "currentAccount_401",
      "codes": [
        "authentication_invalid"
      ]
    },
    "403": {
      "validator": "currentAccount_403",
      "codes": [
        "permission_denied"
      ]
    },
    "429": {
      "validator": "currentAccount_429",
      "codes": [
        "rate_limited"
      ]
    },
    "503": {
      "validator": "currentAccount_503",
      "codes": [
        "dependency_unavailable"
      ]
    }
  },
  "GET /api/v1/accounts/current/role": {
    "200": {
      "validator": "currentRole_200",
      "codes": [
        "ok"
      ]
    },
    "400": {
      "validator": "currentRole_400",
      "codes": [
        "invalid_input"
      ]
    },
    "401": {
      "validator": "currentRole_401",
      "codes": [
        "authentication_invalid"
      ]
    },
    "403": {
      "validator": "currentRole_403",
      "codes": [
        "permission_denied"
      ]
    },
    "429": {
      "validator": "currentRole_429",
      "codes": [
        "rate_limited"
      ]
    },
    "503": {
      "validator": "currentRole_503",
      "codes": [
        "dependency_unavailable"
      ]
    }
  },
  "POST /api/v1/accounts/current/cancellations": {
    "200": {
      "validator": "cancelCurrentAccount_200",
      "codes": [
        "ok"
      ]
    },
    "400": {
      "validator": "cancelCurrentAccount_400",
      "codes": [
        "invalid_input"
      ]
    },
    "401": {
      "validator": "cancelCurrentAccount_401",
      "codes": [
        "authentication_invalid"
      ]
    },
    "403": {
      "validator": "cancelCurrentAccount_403",
      "codes": [
        "permission_denied"
      ]
    },
    "409": {
      "validator": "cancelCurrentAccount_409",
      "codes": [
        "request_mismatch",
        "target_changed"
      ]
    },
    "413": {
      "validator": "cancelCurrentAccount_413",
      "codes": [
        "invalid_input"
      ]
    },
    "415": {
      "validator": "cancelCurrentAccount_415",
      "codes": [
        "invalid_input"
      ]
    },
    "422": {
      "validator": "cancelCurrentAccount_422",
      "codes": [
        "invalid_input"
      ]
    },
    "429": {
      "validator": "cancelCurrentAccount_429",
      "codes": [
        "rate_limited"
      ]
    },
    "500": {
      "validator": "cancelCurrentAccount_500",
      "codes": [
        "internal_error"
      ]
    },
    "503": {
      "validator": "cancelCurrentAccount_503",
      "codes": [
        "dependency_unavailable",
        "outcome_unknown"
      ]
    }
  },
  "GET /api/v1/accounts/current/corrections": {
    "200": {
      "validator": "listCurrentCorrections_200",
      "codes": [
        "ok"
      ]
    },
    "400": {
      "validator": "listCurrentCorrections_400",
      "codes": [
        "invalid_input"
      ]
    },
    "401": {
      "validator": "listCurrentCorrections_401",
      "codes": [
        "authentication_invalid"
      ]
    },
    "403": {
      "validator": "listCurrentCorrections_403",
      "codes": [
        "permission_denied"
      ]
    },
    "429": {
      "validator": "listCurrentCorrections_429",
      "codes": [
        "rate_limited"
      ]
    },
    "503": {
      "validator": "listCurrentCorrections_503",
      "codes": [
        "dependency_unavailable"
      ]
    }
  },
  "GET /api/v1/accounts/current/affiliation": {
    "200": {
      "validator": "currentAffiliation_200",
      "codes": [
        "ok"
      ]
    },
    "400": {
      "validator": "currentAffiliation_400",
      "codes": [
        "invalid_input"
      ]
    },
    "401": {
      "validator": "currentAffiliation_401",
      "codes": [
        "authentication_invalid"
      ]
    },
    "403": {
      "validator": "currentAffiliation_403",
      "codes": [
        "permission_denied"
      ]
    },
    "404": {
      "validator": "currentAffiliation_404",
      "codes": [
        "affiliation_unset"
      ]
    },
    "413": {
      "validator": "currentAffiliation_413",
      "codes": [
        "invalid_input"
      ]
    },
    "429": {
      "validator": "currentAffiliation_429",
      "codes": [
        "rate_limited"
      ]
    },
    "500": {
      "validator": "currentAffiliation_500",
      "codes": [
        "internal_error"
      ]
    },
    "503": {
      "validator": "currentAffiliation_503",
      "codes": [
        "dependency_unavailable"
      ]
    }
  },
  "PUT /api/v1/accounts/current/affiliation": {
    "200": {
      "validator": "createCurrentAffiliation_200",
      "codes": [
        "ok"
      ]
    },
    "400": {
      "validator": "createCurrentAffiliation_400",
      "codes": [
        "invalid_input"
      ]
    },
    "401": {
      "validator": "createCurrentAffiliation_401",
      "codes": [
        "authentication_invalid"
      ]
    },
    "403": {
      "validator": "createCurrentAffiliation_403",
      "codes": [
        "permission_denied"
      ]
    },
    "409": {
      "validator": "createCurrentAffiliation_409",
      "codes": [
        "request_mismatch",
        "state_conflict"
      ]
    },
    "413": {
      "validator": "createCurrentAffiliation_413",
      "codes": [
        "invalid_input"
      ]
    },
    "415": {
      "validator": "createCurrentAffiliation_415",
      "codes": [
        "invalid_input"
      ]
    },
    "422": {
      "validator": "createCurrentAffiliation_422",
      "codes": [
        "invalid_input"
      ]
    },
    "429": {
      "validator": "createCurrentAffiliation_429",
      "codes": [
        "rate_limited"
      ]
    },
    "500": {
      "validator": "createCurrentAffiliation_500",
      "codes": [
        "internal_error"
      ]
    },
    "503": {
      "validator": "createCurrentAffiliation_503",
      "codes": [
        "dependency_unavailable",
        "outcome_unknown"
      ]
    }
  },
  "GET /api/v1/affiliations": {
    "200": {
      "validator": "affiliationValues_200",
      "codes": [
        "ok"
      ]
    },
    "400": {
      "validator": "affiliationValues_400",
      "codes": [
        "invalid_input"
      ]
    },
    "401": {
      "validator": "affiliationValues_401",
      "codes": [
        "authentication_invalid"
      ]
    },
    "403": {
      "validator": "affiliationValues_403",
      "codes": [
        "permission_denied"
      ]
    },
    "413": {
      "validator": "affiliationValues_413",
      "codes": [
        "invalid_input"
      ]
    },
    "429": {
      "validator": "affiliationValues_429",
      "codes": [
        "rate_limited"
      ]
    },
    "500": {
      "validator": "affiliationValues_500",
      "codes": [
        "internal_error"
      ]
    },
    "503": {
      "validator": "affiliationValues_503",
      "codes": [
        "dependency_unavailable"
      ]
    }
  },
  "GET /api/v1/workforce/current/grant": {
    "200": {
      "validator": "currentWorkforceGrant_200",
      "codes": [
        "ok"
      ]
    },
    "400": {
      "validator": "currentWorkforceGrant_400",
      "codes": [
        "invalid_input"
      ]
    },
    "401": {
      "validator": "currentWorkforceGrant_401",
      "codes": [
        "authentication_invalid"
      ]
    },
    "403": {
      "validator": "currentWorkforceGrant_403",
      "codes": [
        "permission_denied"
      ]
    },
    "404": {
      "validator": "currentWorkforceGrant_404",
      "codes": [
        "grant_absent"
      ]
    },
    "413": {
      "validator": "currentWorkforceGrant_413",
      "codes": [
        "invalid_input"
      ]
    },
    "429": {
      "validator": "currentWorkforceGrant_429",
      "codes": [
        "rate_limited"
      ]
    },
    "500": {
      "validator": "currentWorkforceGrant_500",
      "codes": [
        "internal_error"
      ]
    },
    "503": {
      "validator": "currentWorkforceGrant_503",
      "codes": [
        "dependency_unavailable"
      ]
    }
  },
  "POST /api/v1/workforce/grants/candidates": {
    "200": {
      "validator": "workforceGrantCandidate_200",
      "codes": [
        "ok"
      ]
    },
    "400": {
      "validator": "workforceGrantCandidate_400",
      "codes": [
        "invalid_input"
      ]
    },
    "401": {
      "validator": "workforceGrantCandidate_401",
      "codes": [
        "authentication_invalid"
      ]
    },
    "403": {
      "validator": "workforceGrantCandidate_403",
      "codes": [
        "permission_denied"
      ]
    },
    "404": {
      "validator": "workforceGrantCandidate_404",
      "codes": [
        "target_unavailable"
      ]
    },
    "413": {
      "validator": "workforceGrantCandidate_413",
      "codes": [
        "invalid_input"
      ]
    },
    "415": {
      "validator": "workforceGrantCandidate_415",
      "codes": [
        "invalid_input"
      ]
    },
    "422": {
      "validator": "workforceGrantCandidate_422",
      "codes": [
        "invalid_input"
      ]
    },
    "429": {
      "validator": "workforceGrantCandidate_429",
      "codes": [
        "rate_limited"
      ]
    },
    "503": {
      "validator": "workforceGrantCandidate_503",
      "codes": [
        "dependency_unavailable"
      ]
    }
  },
  "POST /api/v1/workforce/grants": {
    "201": {
      "validator": "createWorkforceGrant_201",
      "codes": [
        "ok"
      ]
    },
    "400": {
      "validator": "createWorkforceGrant_400",
      "codes": [
        "invalid_input"
      ]
    },
    "401": {
      "validator": "createWorkforceGrant_401",
      "codes": [
        "authentication_invalid"
      ]
    },
    "403": {
      "validator": "createWorkforceGrant_403",
      "codes": [
        "permission_denied"
      ]
    },
    "404": {
      "validator": "createWorkforceGrant_404",
      "codes": [
        "target_unavailable"
      ]
    },
    "409": {
      "validator": "createWorkforceGrant_409",
      "codes": [
        "request_mismatch",
        "target_changed",
        "role_changed",
        "already_authorized"
      ]
    },
    "413": {
      "validator": "createWorkforceGrant_413",
      "codes": [
        "invalid_input"
      ]
    },
    "415": {
      "validator": "createWorkforceGrant_415",
      "codes": [
        "invalid_input"
      ]
    },
    "422": {
      "validator": "createWorkforceGrant_422",
      "codes": [
        "invalid_input",
        "account_not_ready"
      ]
    },
    "429": {
      "validator": "createWorkforceGrant_429",
      "codes": [
        "rate_limited"
      ]
    },
    "500": {
      "validator": "createWorkforceGrant_500",
      "codes": [
        "internal_error"
      ]
    },
    "503": {
      "validator": "createWorkforceGrant_503",
      "codes": [
        "dependency_unavailable",
        "outcome_unknown"
      ]
    }
  },
  "GET /api/v1/workforce/current/numbers": {
    "200": {
      "validator": "currentWorkforceNumber_200",
      "codes": [
        "ok"
      ]
    },
    "400": {
      "validator": "currentWorkforceNumber_400",
      "codes": [
        "invalid_input"
      ]
    },
    "401": {
      "validator": "currentWorkforceNumber_401",
      "codes": [
        "authentication_invalid"
      ]
    },
    "403": {
      "validator": "currentWorkforceNumber_403",
      "codes": [
        "permission_denied"
      ]
    },
    "413": {
      "validator": "currentWorkforceNumber_413",
      "codes": [
        "invalid_input"
      ]
    },
    "429": {
      "validator": "currentWorkforceNumber_429",
      "codes": [
        "rate_limited"
      ]
    },
    "500": {
      "validator": "currentWorkforceNumber_500",
      "codes": [
        "internal_error"
      ]
    },
    "503": {
      "validator": "currentWorkforceNumber_503",
      "codes": [
        "dependency_unavailable"
      ]
    }
  },
  "GET /api/v1/workforce/directories": {
    "200": {
      "validator": "readWorkforceDirectorySelection_200",
      "codes": [
        "ok"
      ]
    },
    "400": {
      "validator": "readWorkforceDirectorySelection_400",
      "codes": [
        "invalid_input"
      ]
    },
    "401": {
      "validator": "readWorkforceDirectorySelection_401",
      "codes": [
        "authentication_invalid"
      ]
    },
    "403": {
      "validator": "readWorkforceDirectorySelection_403",
      "codes": [
        "permission_denied"
      ]
    },
    "413": {
      "validator": "readWorkforceDirectorySelection_413",
      "codes": [
        "invalid_input"
      ]
    },
    "429": {
      "validator": "readWorkforceDirectorySelection_429",
      "codes": [
        "rate_limited"
      ]
    },
    "500": {
      "validator": "readWorkforceDirectorySelection_500",
      "codes": [
        "internal_error"
      ]
    },
    "503": {
      "validator": "readWorkforceDirectorySelection_503",
      "codes": [
        "dependency_unavailable"
      ]
    }
  },
  "POST /api/v1/workforce/numbers": {
    "201": {
      "validator": "allocateWorkforceNumber_201",
      "codes": [
        "ok"
      ]
    },
    "400": {
      "validator": "allocateWorkforceNumber_400",
      "codes": [
        "invalid_input"
      ]
    },
    "401": {
      "validator": "allocateWorkforceNumber_401",
      "codes": [
        "authentication_invalid"
      ]
    },
    "403": {
      "validator": "allocateWorkforceNumber_403",
      "codes": [
        "permission_denied"
      ]
    },
    "409": {
      "validator": "allocateWorkforceNumber_409",
      "codes": [
        "number_present",
        "number_pool_exhausted",
        "request_mismatch"
      ]
    },
    "413": {
      "validator": "allocateWorkforceNumber_413",
      "codes": [
        "invalid_input"
      ]
    },
    "415": {
      "validator": "allocateWorkforceNumber_415",
      "codes": [
        "invalid_input"
      ]
    },
    "422": {
      "validator": "allocateWorkforceNumber_422",
      "codes": [
        "invalid_input"
      ]
    },
    "429": {
      "validator": "allocateWorkforceNumber_429",
      "codes": [
        "rate_limited"
      ]
    },
    "500": {
      "validator": "allocateWorkforceNumber_500",
      "codes": [
        "internal_error"
      ]
    },
    "503": {
      "validator": "allocateWorkforceNumber_503",
      "codes": [
        "dependency_unavailable",
        "outcome_unknown"
      ]
    }
  },
  "GET /api/v1/workforce/application-template": {
    "200": {
      "validator": "workforceApplicationTemplate_200",
      "codes": [
        "ok"
      ]
    },
    "400": {
      "validator": "workforceApplicationTemplate_400",
      "codes": [
        "invalid_input"
      ]
    },
    "401": {
      "validator": "workforceApplicationTemplate_401",
      "codes": [
        "authentication_invalid"
      ]
    },
    "403": {
      "validator": "workforceApplicationTemplate_403",
      "codes": [
        "permission_denied"
      ]
    },
    "413": {
      "validator": "workforceApplicationTemplate_413",
      "codes": [
        "invalid_input"
      ]
    },
    "429": {
      "validator": "workforceApplicationTemplate_429",
      "codes": [
        "rate_limited"
      ]
    },
    "500": {
      "validator": "workforceApplicationTemplate_500",
      "codes": [
        "internal_error"
      ]
    },
    "503": {
      "validator": "workforceApplicationTemplate_503",
      "codes": [
        "dependency_unavailable"
      ]
    }
  },
  "POST /api/v1/workforce/application-photos": {
    "201": {
      "validator": "workforceApplicationPhoto_201",
      "codes": [
        "ok"
      ]
    },
    "400": {
      "validator": "workforceApplicationPhoto_400",
      "codes": [
        "invalid_input"
      ]
    },
    "401": {
      "validator": "workforceApplicationPhoto_401",
      "codes": [
        "authentication_invalid"
      ]
    },
    "403": {
      "validator": "workforceApplicationPhoto_403",
      "codes": [
        "permission_denied"
      ]
    },
    "413": {
      "validator": "workforceApplicationPhoto_413",
      "codes": [
        "invalid_input"
      ]
    },
    "415": {
      "validator": "workforceApplicationPhoto_415",
      "codes": [
        "invalid_input"
      ]
    },
    "422": {
      "validator": "workforceApplicationPhoto_422",
      "codes": [
        "invalid_input"
      ]
    },
    "429": {
      "validator": "workforceApplicationPhoto_429",
      "codes": [
        "rate_limited"
      ]
    },
    "500": {
      "validator": "workforceApplicationPhoto_500",
      "codes": [
        "internal_error"
      ]
    },
    "503": {
      "validator": "workforceApplicationPhoto_503",
      "codes": [
        "dependency_unavailable",
        "outcome_unknown"
      ]
    }
  },
  "GET /api/v1/workforce/application-resources/{id}": {
    "400": {
      "validator": "workforceApplicationResource_400",
      "codes": [
        "invalid_input"
      ]
    },
    "401": {
      "validator": "workforceApplicationResource_401",
      "codes": [
        "authentication_invalid"
      ]
    },
    "403": {
      "validator": "workforceApplicationResource_403",
      "codes": [
        "permission_denied"
      ]
    },
    "404": {
      "validator": "workforceApplicationResource_404",
      "codes": [
        "target_unavailable"
      ]
    },
    "413": {
      "validator": "workforceApplicationResource_413",
      "codes": [
        "invalid_input"
      ]
    },
    "429": {
      "validator": "workforceApplicationResource_429",
      "codes": [
        "rate_limited"
      ]
    },
    "500": {
      "validator": "workforceApplicationResource_500",
      "codes": [
        "internal_error"
      ]
    },
    "503": {
      "validator": "workforceApplicationResource_503",
      "codes": [
        "dependency_unavailable"
      ]
    }
  },
  "POST /api/v1/workforce/applications/preview": {
    "200": {
      "validator": "workforceApplicationPreview_200",
      "codes": [
        "ok"
      ]
    },
    "400": {
      "validator": "workforceApplicationPreview_400",
      "codes": [
        "invalid_input"
      ]
    },
    "401": {
      "validator": "workforceApplicationPreview_401",
      "codes": [
        "authentication_invalid"
      ]
    },
    "403": {
      "validator": "workforceApplicationPreview_403",
      "codes": [
        "permission_denied"
      ]
    },
    "404": {
      "validator": "workforceApplicationPreview_404",
      "codes": [
        "target_unavailable"
      ]
    },
    "409": {
      "validator": "workforceApplicationPreview_409",
      "codes": [
        "number_required"
      ]
    },
    "413": {
      "validator": "workforceApplicationPreview_413",
      "codes": [
        "invalid_input"
      ]
    },
    "415": {
      "validator": "workforceApplicationPreview_415",
      "codes": [
        "invalid_input"
      ]
    },
    "422": {
      "validator": "workforceApplicationPreview_422",
      "codes": [
        "selection_unavailable",
        "missing_glyph",
        "text_overflow"
      ]
    },
    "429": {
      "validator": "workforceApplicationPreview_429",
      "codes": [
        "rate_limited"
      ]
    },
    "500": {
      "validator": "workforceApplicationPreview_500",
      "codes": [
        "internal_error"
      ]
    },
    "503": {
      "validator": "workforceApplicationPreview_503",
      "codes": [
        "dependency_unavailable"
      ]
    }
  },
  "POST /api/v1/workforce/applications": {
    "201": {
      "validator": "workforceApplicationSubmit_201",
      "codes": [
        "ok"
      ]
    },
    "400": {
      "validator": "workforceApplicationSubmit_400",
      "codes": [
        "invalid_input"
      ]
    },
    "401": {
      "validator": "workforceApplicationSubmit_401",
      "codes": [
        "authentication_invalid"
      ]
    },
    "403": {
      "validator": "workforceApplicationSubmit_403",
      "codes": [
        "permission_denied"
      ]
    },
    "404": {
      "validator": "workforceApplicationSubmit_404",
      "codes": [
        "target_unavailable"
      ]
    },
    "409": {
      "validator": "workforceApplicationSubmit_409",
      "codes": [
        "data_changed",
        "number_required"
      ]
    },
    "413": {
      "validator": "workforceApplicationSubmit_413",
      "codes": [
        "invalid_input"
      ]
    },
    "415": {
      "validator": "workforceApplicationSubmit_415",
      "codes": [
        "invalid_input"
      ]
    },
    "422": {
      "validator": "workforceApplicationSubmit_422",
      "codes": [
        "selection_unavailable",
        "missing_glyph",
        "text_overflow"
      ]
    },
    "429": {
      "validator": "workforceApplicationSubmit_429",
      "codes": [
        "rate_limited"
      ]
    },
    "500": {
      "validator": "workforceApplicationSubmit_500",
      "codes": [
        "internal_error"
      ]
    },
    "503": {
      "validator": "workforceApplicationSubmit_503",
      "codes": [
        "dependency_unavailable",
        "outcome_unknown"
      ]
    }
  },
  "GET /api/v1/workforce/current/applications": {
    "200": {
      "validator": "workforceCurrentApplications_200",
      "codes": [
        "ok"
      ]
    },
    "400": {
      "validator": "workforceCurrentApplications_400",
      "codes": [
        "invalid_input"
      ]
    },
    "401": {
      "validator": "workforceCurrentApplications_401",
      "codes": [
        "authentication_invalid"
      ]
    },
    "403": {
      "validator": "workforceCurrentApplications_403",
      "codes": [
        "permission_denied"
      ]
    },
    "413": {
      "validator": "workforceCurrentApplications_413",
      "codes": [
        "invalid_input"
      ]
    },
    "429": {
      "validator": "workforceCurrentApplications_429",
      "codes": [
        "rate_limited"
      ]
    },
    "500": {
      "validator": "workforceCurrentApplications_500",
      "codes": [
        "internal_error"
      ]
    },
    "503": {
      "validator": "workforceCurrentApplications_503",
      "codes": [
        "dependency_unavailable"
      ]
    }
  },
  "GET /api/v1/workforce/current/applications/{id}": {
    "200": {
      "validator": "workforceCurrentApplication_200",
      "codes": [
        "ok"
      ]
    },
    "400": {
      "validator": "workforceCurrentApplication_400",
      "codes": [
        "invalid_input"
      ]
    },
    "401": {
      "validator": "workforceCurrentApplication_401",
      "codes": [
        "authentication_invalid"
      ]
    },
    "403": {
      "validator": "workforceCurrentApplication_403",
      "codes": [
        "permission_denied"
      ]
    },
    "404": {
      "validator": "workforceCurrentApplication_404",
      "codes": [
        "target_unavailable"
      ]
    },
    "413": {
      "validator": "workforceCurrentApplication_413",
      "codes": [
        "invalid_input"
      ]
    },
    "429": {
      "validator": "workforceCurrentApplication_429",
      "codes": [
        "rate_limited"
      ]
    },
    "500": {
      "validator": "workforceCurrentApplication_500",
      "codes": [
        "internal_error"
      ]
    },
    "503": {
      "validator": "workforceCurrentApplication_503",
      "codes": [
        "dependency_unavailable"
      ]
    }
  },
  "POST /api/v1/workforce/grants/{id}/terminations": {
    "201": {
      "validator": "terminateWorkforceGrant_201",
      "codes": [
        "ok"
      ]
    },
    "400": {
      "validator": "terminateWorkforceGrant_400",
      "codes": [
        "invalid_input"
      ]
    },
    "401": {
      "validator": "terminateWorkforceGrant_401",
      "codes": [
        "authentication_invalid"
      ]
    },
    "403": {
      "validator": "terminateWorkforceGrant_403",
      "codes": [
        "permission_denied"
      ]
    },
    "404": {
      "validator": "terminateWorkforceGrant_404",
      "codes": [
        "target_unavailable"
      ]
    },
    "409": {
      "validator": "terminateWorkforceGrant_409",
      "codes": [
        "request_mismatch",
        "target_changed",
        "state_conflict"
      ]
    },
    "413": {
      "validator": "terminateWorkforceGrant_413",
      "codes": [
        "invalid_input"
      ]
    },
    "415": {
      "validator": "terminateWorkforceGrant_415",
      "codes": [
        "invalid_input"
      ]
    },
    "422": {
      "validator": "terminateWorkforceGrant_422",
      "codes": [
        "invalid_input"
      ]
    },
    "429": {
      "validator": "terminateWorkforceGrant_429",
      "codes": [
        "rate_limited"
      ]
    },
    "500": {
      "validator": "terminateWorkforceGrant_500",
      "codes": [
        "internal_error"
      ]
    },
    "503": {
      "validator": "terminateWorkforceGrant_503",
      "codes": [
        "dependency_unavailable",
        "outcome_unknown"
      ]
    }
  },
  "DELETE /api/v1/sessions/current": {
    "200": {
      "validator": "revokeCurrentSession_200",
      "codes": [
        "ok"
      ]
    },
    "400": {
      "validator": "revokeCurrentSession_400",
      "codes": [
        "invalid_input"
      ]
    },
    "401": {
      "validator": "revokeCurrentSession_401",
      "codes": [
        "authentication_invalid"
      ]
    },
    "403": {
      "validator": "revokeCurrentSession_403",
      "codes": [
        "permission_denied"
      ]
    },
    "409": {
      "validator": "revokeCurrentSession_409",
      "codes": [
        "request_mismatch"
      ]
    },
    "413": {
      "validator": "revokeCurrentSession_413",
      "codes": [
        "invalid_input"
      ]
    },
    "429": {
      "validator": "revokeCurrentSession_429",
      "codes": [
        "rate_limited"
      ]
    },
    "500": {
      "validator": "revokeCurrentSession_500",
      "codes": [
        "internal_error"
      ]
    },
    "503": {
      "validator": "revokeCurrentSession_503",
      "codes": [
        "dependency_unavailable",
        "outcome_unknown"
      ]
    }
  },
  "GET /api/v1/revocations": {
    "200": {
      "validator": "readRevocations_200",
      "codes": [
        "ok"
      ]
    },
    "400": {
      "validator": "readRevocations_400",
      "codes": [
        "invalid_input"
      ]
    },
    "401": {
      "validator": "readRevocations_401",
      "codes": [
        "authentication_invalid"
      ]
    },
    "403": {
      "validator": "readRevocations_403",
      "codes": [
        "permission_denied"
      ]
    },
    "413": {
      "validator": "readRevocations_413",
      "codes": [
        "invalid_input"
      ]
    },
    "429": {
      "validator": "readRevocations_429",
      "codes": [
        "rate_limited"
      ]
    },
    "500": {
      "validator": "readRevocations_500",
      "codes": [
        "internal_error"
      ]
    },
    "503": {
      "validator": "readRevocations_503",
      "codes": [
        "dependency_unavailable"
      ]
    }
  },
  "POST /api/v1/accounts/current/codes": {
    "200": {
      "validator": "sendCurrentMobileCode_200",
      "codes": [
        "ok"
      ]
    },
    "400": {
      "validator": "sendCurrentMobileCode_400",
      "codes": [
        "invalid_input"
      ]
    },
    "401": {
      "validator": "sendCurrentMobileCode_401",
      "codes": [
        "authentication_invalid"
      ]
    },
    "403": {
      "validator": "sendCurrentMobileCode_403",
      "codes": [
        "verification_invalid",
        "permission_denied"
      ]
    },
    "409": {
      "validator": "sendCurrentMobileCode_409",
      "codes": [
        "request_mismatch",
        "target_changed"
      ]
    },
    "410": {
      "validator": "sendCurrentMobileCode_410",
      "codes": [
        "verification_required"
      ]
    },
    "413": {
      "validator": "sendCurrentMobileCode_413",
      "codes": [
        "invalid_input"
      ]
    },
    "415": {
      "validator": "sendCurrentMobileCode_415",
      "codes": [
        "invalid_input"
      ]
    },
    "422": {
      "validator": "sendCurrentMobileCode_422",
      "codes": [
        "invalid_input"
      ]
    },
    "429": {
      "validator": "sendCurrentMobileCode_429",
      "codes": [
        "rate_limited"
      ]
    },
    "500": {
      "validator": "sendCurrentMobileCode_500",
      "codes": [
        "internal_error"
      ]
    },
    "503": {
      "validator": "sendCurrentMobileCode_503",
      "codes": [
        "dependency_unavailable",
        "outcome_unknown"
      ]
    }
  },
  "POST /api/v1/mobile-changes": {
    "201": {
      "validator": "createMobileTransfer_201",
      "codes": [
        "ok"
      ]
    },
    "400": {
      "validator": "createMobileTransfer_400",
      "codes": [
        "invalid_input"
      ]
    },
    "401": {
      "validator": "createMobileTransfer_401",
      "codes": [
        "authentication_invalid"
      ]
    },
    "403": {
      "validator": "createMobileTransfer_403",
      "codes": [
        "verification_invalid",
        "permission_denied"
      ]
    },
    "409": {
      "validator": "createMobileTransfer_409",
      "codes": [
        "request_mismatch"
      ]
    },
    "410": {
      "validator": "createMobileTransfer_410",
      "codes": [
        "verification_required"
      ]
    },
    "413": {
      "validator": "createMobileTransfer_413",
      "codes": [
        "invalid_input"
      ]
    },
    "415": {
      "validator": "createMobileTransfer_415",
      "codes": [
        "invalid_input"
      ]
    },
    "422": {
      "validator": "createMobileTransfer_422",
      "codes": [
        "invalid_input"
      ]
    },
    "429": {
      "validator": "createMobileTransfer_429",
      "codes": [
        "rate_limited"
      ]
    },
    "500": {
      "validator": "createMobileTransfer_500",
      "codes": [
        "internal_error"
      ]
    },
    "503": {
      "validator": "createMobileTransfer_503",
      "codes": [
        "dependency_unavailable",
        "outcome_unknown"
      ]
    }
  },
  "POST /api/v1/mobile-changes/{id}/codes": {
    "200": {
      "validator": "sendNewMobileCode_200",
      "codes": [
        "ok"
      ]
    },
    "400": {
      "validator": "sendNewMobileCode_400",
      "codes": [
        "invalid_input"
      ]
    },
    "401": {
      "validator": "sendNewMobileCode_401",
      "codes": [
        "authentication_invalid"
      ]
    },
    "403": {
      "validator": "sendNewMobileCode_403",
      "codes": [
        "verification_invalid",
        "permission_denied"
      ]
    },
    "404": {
      "validator": "sendNewMobileCode_404",
      "codes": [
        "target_unavailable"
      ]
    },
    "409": {
      "validator": "sendNewMobileCode_409",
      "codes": [
        "request_mismatch",
        "state_conflict"
      ]
    },
    "410": {
      "validator": "sendNewMobileCode_410",
      "codes": [
        "verification_required"
      ]
    },
    "413": {
      "validator": "sendNewMobileCode_413",
      "codes": [
        "invalid_input"
      ]
    },
    "415": {
      "validator": "sendNewMobileCode_415",
      "codes": [
        "invalid_input"
      ]
    },
    "422": {
      "validator": "sendNewMobileCode_422",
      "codes": [
        "invalid_input"
      ]
    },
    "429": {
      "validator": "sendNewMobileCode_429",
      "codes": [
        "rate_limited"
      ]
    },
    "500": {
      "validator": "sendNewMobileCode_500",
      "codes": [
        "internal_error"
      ]
    },
    "503": {
      "validator": "sendNewMobileCode_503",
      "codes": [
        "dependency_unavailable",
        "outcome_unknown"
      ]
    }
  },
  "PUT /api/v1/accounts/current/mobile": {
    "200": {
      "validator": "completeMobileTransfer_200",
      "codes": [
        "ok"
      ]
    },
    "400": {
      "validator": "completeMobileTransfer_400",
      "codes": [
        "invalid_input"
      ]
    },
    "401": {
      "validator": "completeMobileTransfer_401",
      "codes": [
        "authentication_invalid"
      ]
    },
    "403": {
      "validator": "completeMobileTransfer_403",
      "codes": [
        "verification_invalid",
        "permission_denied"
      ]
    },
    "409": {
      "validator": "completeMobileTransfer_409",
      "codes": [
        "request_mismatch",
        "state_conflict",
        "mobile_taken"
      ]
    },
    "410": {
      "validator": "completeMobileTransfer_410",
      "codes": [
        "verification_required"
      ]
    },
    "413": {
      "validator": "completeMobileTransfer_413",
      "codes": [
        "invalid_input"
      ]
    },
    "415": {
      "validator": "completeMobileTransfer_415",
      "codes": [
        "invalid_input"
      ]
    },
    "422": {
      "validator": "completeMobileTransfer_422",
      "codes": [
        "invalid_input"
      ]
    },
    "429": {
      "validator": "completeMobileTransfer_429",
      "codes": [
        "rate_limited"
      ]
    },
    "500": {
      "validator": "completeMobileTransfer_500",
      "codes": [
        "internal_error"
      ]
    },
    "503": {
      "validator": "completeMobileTransfer_503",
      "codes": [
        "dependency_unavailable",
        "outcome_unknown"
      ]
    }
  },
  "GET /api/v1/mobile-changes/{id}": {
    "200": {
      "validator": "readMobileTransfer_200",
      "codes": [
        "ok"
      ]
    },
    "400": {
      "validator": "readMobileTransfer_400",
      "codes": [
        "invalid_input"
      ]
    },
    "401": {
      "validator": "readMobileTransfer_401",
      "codes": [
        "authentication_invalid"
      ]
    },
    "403": {
      "validator": "readMobileTransfer_403",
      "codes": [
        "permission_denied"
      ]
    },
    "404": {
      "validator": "readMobileTransfer_404",
      "codes": [
        "target_unavailable"
      ]
    },
    "413": {
      "validator": "readMobileTransfer_413",
      "codes": [
        "invalid_input"
      ]
    },
    "429": {
      "validator": "readMobileTransfer_429",
      "codes": [
        "rate_limited"
      ]
    },
    "500": {
      "validator": "readMobileTransfer_500",
      "codes": [
        "internal_error"
      ]
    },
    "503": {
      "validator": "readMobileTransfer_503",
      "codes": [
        "dependency_unavailable"
      ]
    }
  }
};
