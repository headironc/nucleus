export interface paths {
    "/api/v1/codes": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        put?: never;
        /**
         * Send or recover a retained C1 using its original Operation-Id, mobile and token.
         *     Requires a JSON media type, at most 8192 bytes, and an exact configured Origin
         *     header. No current session is required.
         */
        post: operations["sendCode"];
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/v1/settings": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        /** @description Read only the public widget site key. See docs/http/site.md. */
        get: operations["readPublicSettings"];
        put?: never;
        post?: never;
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/v1/sessions": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        put?: never;
        /** @description Message credentials require the configured website Origin and deliver a cookie. Mobile credentials contain a getPhoneNumber code, use the configured mini-program identity, require no website Origin, and deliver an opaque Bearer token. Missing mini-program configuration returns dependency_unavailable without claiming or exchanging the code. */
        post: operations["createSession"];
        /** @description Revokes every retained unrevoked session of the authenticated account across website and mini-program sources, including expired sessions. Accepts a selected website cookie with exact Origin, or a unique mini-program Bearer token without website Origin. Mixed credentials are rejected. Only website success clears its selected cookie. Later sessions are not added; a new valid session of the same account can read the original with GET /revocations. */
        delete: operations["revokeAllSessions"];
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/v1/registrations": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        put?: never;
        /** @description The verified original credential or receipt determines the source. Website sources require the configured Origin and deliver the original cookie; mini-program phone sources require no website Origin and deliver session id, opaque Bearer token and expiry. Failed responses never set a cookie. */
        post: operations["register"];
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/v1/accounts/current": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        /** @description Accepts a selected website cookie with same-origin checks, or a unique opaque mini-program Bearer token. Bearer must not be mixed with Session-Id or any session cookie and does not require website Origin. Website Origin must match when supplied; absent Origin requires Sec-Fetch-Site: same-origin. */
        get: operations["currentAccount"];
        put?: never;
        post?: never;
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/v1/accounts/current/role": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        /** @description Read the current website account's institutional role. Contract: docs/access/roles.md#本人角色与定义列表. */
        get: operations["currentRole"];
        put?: never;
        post?: never;
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/v1/accounts/current/cancellations": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        put?: never;
        /** @description Cancels the confirmed current account. See docs/identity/cancellation.md for qualification, atomic consequences and recovery. */
        post: operations["cancelCurrentAccount"];
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/v1/accounts/current/corrections": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        /** @description Returns the original session's own person history without requiring a management role. Records are ordered by personal sequence descending, then ID descending. Before and after names are complete; ID numbers contain only their last four characters. No operator is disclosed. Current identity, count and the complete page share one three-second read-only snapshot. Origin must match when supplied; absent Origin requires Sec-Fetch-Site: same-origin. Any supplied Sec-Fetch-Site must be same-origin. */
        get: operations["listCurrentCorrections"];
        put?: never;
        post?: never;
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/v1/accounts/current/affiliation": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        /** @description Read the current person's value and createdAt without institutional or Workforce permission. No query or body is accepted. Origin must match when supplied; absent Origin requires Sec-Fetch-Site: same-origin. Supplied Sec-Fetch-Site must be same-origin. Preserves the session cookie. Read contract: docs/center/affiliation.md#http-契约. */
        get: operations["currentAffiliation"];
        /** @description Record the current person's first affiliation without institutional or Workforce permission. Requires exact Origin; supplied Sec-Fetch-Site must be same-origin. Operation-Id is diagnostic only, without deduplication or replay. Success returns only code: ok and preserves the session cookie. Refresh current state after an unknown outcome; never automatically repeat PUT. Rules: docs/center/affiliation.md#http-契约. */
        put: operations["createCurrentAffiliation"];
        post?: never;
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/v1/affiliations": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        /** @description Read all 13 fixed affiliation codes and display names in ascending code order for first fill and display. Requires a current valid website session without institutional or Workforce permission. Reads no person's affiliation. No pagination, query or body is accepted. Origin must match when supplied; absent Origin requires Sec-Fetch-Site: same-origin. Supplied Sec-Fetch-Site must be same-origin. Preserves the session cookie and expiry. Vocabulary: docs/center/values.md. */
        get: operations["affiliationValues"];
        put?: never;
        post?: never;
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/v1/workforce/current/grant": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        /** @description Read the current person's latest Workforce Grant and status. Contract: docs/access/grants.md#http-入口. */
        get: operations["currentWorkforceGrant"];
        put?: never;
        post?: never;
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/v1/workforce/grants/candidates": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        put?: never;
        /** @description Confirm a Workforce admission target and eligible roles through the selected authority. Contract: docs/access/grants.md#http-入口. */
        post: operations["workforceGrantCandidate"];
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/v1/workforce/grants": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        put?: never;
        /** @description Admit the confirmed target to Workforce and retain the original Grant and first assignment. Contract: docs/access/grants.md#http-入口. */
        post: operations["createWorkforceGrant"];
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/v1/workforce/current/numbers": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        /** @description Read this person's current number or null. Business rules and HTTP contract: [Workforce numbers](docs/workforce/numbers.md#http-入口). */
        get: operations["currentWorkforceNumber"];
        put?: never;
        post?: never;
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/v1/workforce/numbers": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        put?: never;
        /** @description Allocate this member's first number for the current year. Business rules and HTTP contract: [Workforce numbers](docs/workforce/numbers.md#http-入口). */
        post: operations["allocateWorkforceNumber"];
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/v1/workforce/grants/{id}/terminations": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        put?: never;
        /** @description End the exact Grant through self exit or confirmed management action. Contract: docs/access/grants.md#人工结束与注销. */
        post: operations["terminateWorkforceGrant"];
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/v1/sessions/current": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        put?: never;
        post?: never;
        /** @description Revokes the currently authenticated session. Website cookies require exact Origin and same-origin Sec-Fetch-Site when supplied. Mini-program Bearer requires no website Origin and cannot be mixed with Session-Id or a session cookie. Only website success clears its selected cookie. A revoked proof cannot replay DELETE; a new session of the same account can read the original with GET /revocations. */
        delete: operations["revokeCurrentSession"];
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/v1/revocations": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        /** @description Requires a valid website cookie or mini-program Bearer session of the original account. Reads the permanent logout scope, initiatorId and fixed session set in session-ID order, even after session cleanup. Missing and other-account operations return original: null and an empty list. This read never executes logout or sets a cookie. Website Origin must match when supplied; absent Origin requires same-origin Sec-Fetch-Site. Bearer does not require website Origin and cannot be mixed with website credentials. */
        get: operations["readRevocations"];
        put?: never;
        post?: never;
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/v1/accounts/current/codes": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        put?: never;
        /** @description Verify and send to the current website account phone. Replays preserve the original person and session. Authentication and mobile verification share the same sending budgets. */
        post: operations["sendCurrentMobileCode"];
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/v1/mobile-changes": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        put?: never;
        /** @description Prove the current account phone with its latest original-phone code. A successful transfer lasts ten minutes and is bound to this website session. Replaying the original input neither creates another transfer nor counts a wrong code again. */
        post: operations["createMobileTransfer"];
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/v1/mobile-changes/{id}/codes": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        put?: never;
        /** @description Send a verification code to a different phone using the website session that established the current transfer. New sends require an unexpired latest transfer and unchanged target identity. Assisted sends and original reads require current institution.accounts and a strictly lower target; paused targets remain eligible. Phone occupancy is not disclosed. Original results remain readable without another supplier call. */
        post: operations["sendNewMobileCode"];
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/v1/accounts/current/mobile": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        /** @description Replace the current phone after its latest new-phone verification. Completion revokes all existing account sessions. Authenticate with the new phone before reading the transfer status. */
        put: operations["completeMobileTransfer"];
        post?: never;
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/v1/mobile-changes/{id}": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        /** @description Read a mobile change's minimal status as its person, or as a current manager with institution.accounts and a strictly higher institution level. Paused targets remain readable. Missing and unauthorized targets both return 404. Completed results survive short-term record cleanup. Origin must match when supplied; absent Origin requires Sec-Fetch-Site: same-origin. Any supplied Sec-Fetch-Site must be same-origin. */
        get: operations["readMobileTransfer"];
        put?: never;
        post?: never;
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
}
export type webhooks = Record<string, never>;
export interface components {
    schemas: {
        Input: {
            mobile: string;
            token: string;
        };
        Receipt: {
            /** @example ok */
            code: string;
            message: components["schemas"]["Message"];
        };
        Message: {
            delivery: components["schemas"]["Delivery"];
            /** Format: date-time */
            expiresAt: string;
            id: components["schemas"]["message.Id"];
        };
        /** @enum {string} */
        Delivery: "sending" | "sent" | "failed" | "unknown";
        /**
         * Format: uuid
         * @description The server-generated identity of a C1 message and its paired R1 check.
         */
        "message.Id": string;
        /** @description An error response with an extensible code. Clients must tolerate unknown values. */
        Failure: {
            code: string;
        };
        PublicSettingsResponse: {
            /** @example ok */
            code: string;
            settings: components["schemas"]["PublicSettings"];
        };
        PublicSettings: {
            challenge: components["schemas"]["PublicChallengeSettings"];
        };
        PublicChallengeSettings: {
            turnstile: components["schemas"]["PublicTurnstileSettings"];
        };
        PublicTurnstileSettings: {
            siteKey: string;
        };
        "session.Input": {
            credential: components["schemas"]["session.Credential"];
            secret: string;
        };
        "session.Credential": {
            code: string;
            id: components["schemas"]["message.Id"];
            mobile: string;
            /** @enum {string} */
            type: "message";
        } | {
            code: string;
            /** @enum {string} */
            type: "mobile";
        };
        "session.Receipt": {
            code: string;
            session: components["schemas"]["session.Delivery"];
        } | {
            code: string;
            registration: components["schemas"]["session.Registration"];
        };
        "session.Delivery": components["schemas"]["session.Session"] | components["schemas"]["session.Mini"];
        "session.Session": {
            /** Format: date-time */
            expiresAt: string;
            id: components["schemas"]["session.Id"];
        };
        /** Format: uuid */
        "session.Id": string;
        "session.Mini": {
            /** Format: date-time */
            expiresAt: string;
            id: components["schemas"]["session.Id"];
            token: string;
        };
        "session.Registration": {
            /** Format: date-time */
            expiresAt: string;
            id: components["schemas"]["registration.Id"];
            token: string;
        };
        /** Format: uuid */
        "registration.Id": string;
        "revocation.Receipt": {
            code: string;
            completion: components["schemas"]["revocation.Completion"];
        };
        "revocation.Completion": {
            /** Format: int64 */
            count: number;
            id: components["schemas"]["operation.Id"];
            /** Format: date-time */
            revokedAt: string;
        };
        /**
         * Format: uuid
         * @description An Identity operation identifier. Business originals use client-supplied IDs.
         */
        "operation.Id": string;
        "registration.Input": {
            id: components["schemas"]["registration.Id"];
            idNumber: string;
            name: components["schemas"]["person.Name"];
            token: string;
        };
        /** @description A personal name with 1 to 64 Unicode scalars after trimming whitespace and applying NFC. */
        "person.Name": string;
        "registration.Receipt": {
            code: string;
            session: components["schemas"]["session.Delivery"];
        };
        "current.Receipt": {
            account: components["schemas"]["current.Account"];
            code: string;
        };
        "current.Account": {
            avatar: components["schemas"]["avatar.Current"];
            binding: components["schemas"]["current.Binding"] | null;
            id: components["schemas"]["account.Id"];
            idNumber: components["schemas"]["person.number.Suffix"];
            name: string;
            personId: components["schemas"]["person.Id"];
            revisionId: components["schemas"]["person.revision.Id"];
        };
        "avatar.Current": {
            /** Format: date-time */
            expiresAt: string;
            /** Format: int32 */
            height: number;
            /** Format: uri */
            href: string;
            mediaType: string;
            revision: components["schemas"]["avatar.Revision"];
            /** Format: int32 */
            width: number;
        };
        /** Format: uuid */
        "avatar.Revision": string;
        "current.Binding": {
            revision: components["schemas"]["wechat.Revision"];
        };
        /** Format: uuid */
        "wechat.Revision": string;
        /** Format: uuid */
        "account.Id": string;
        "person.number.Suffix": {
            suffix: string;
        };
        /** Format: uuid */
        "person.Id": string;
        /** Format: uuid */
        "person.revision.Id": string;
        "role.Receipt": {
            code: string;
            role: components["schemas"]["role.Role"];
        };
        "role.Role": {
            changeId: components["schemas"]["role.change.Id"];
            id: components["schemas"]["role.Id"];
            kind: components["schemas"]["role.Kind"];
            level: components["schemas"]["role.Level"];
            name: components["schemas"]["role.Name"];
            permissions: components["schemas"]["capability.Code"][];
        };
        /** Format: uuid */
        "role.change.Id": string;
        /** Format: uuid */
        "role.Id": string;
        /** @enum {string} */
        "role.Kind": "default" | "full" | "custom";
        /** Format: int32 */
        "role.Level": number;
        "role.Name": string;
        /** @enum {string} */
        "capability.Code": "institution.articles" | "institution.roles" | "institution.assignments" | "institution.workforce" | "institution.accounts" | "institution.corrections" | "institution.affiliations.read" | "institution.affiliations.correct" | "workforce.roles" | "workforce.assignments" | "workforce.admission" | "workforce.suspension" | "workforce.termination" | "workforce.numbers" | "workforce.directory" | "workforce.templates" | "workforce.approval" | "workforce.production" | "workforce.delivery";
        "cancellation.Input": {
            accountId: components["schemas"]["account.Id"];
            personId: components["schemas"]["person.Id"];
            revisionId: components["schemas"]["person.revision.Id"];
        };
        "cancellation.Receipt": {
            cancellation: components["schemas"]["cancellation.Cancellation"];
            code: string;
        };
        "cancellation.Cancellation": {
            /** Format: date-time */
            cancelledAt: string;
            id: components["schemas"]["cancellation.Id"];
        };
        /** Format: uuid */
        "cancellation.Id": string;
        "correction.History": {
            code: string;
            corrections: components["schemas"]["correction.history.Correction"][];
            /** Format: int64 */
            count: number;
        };
        "correction.history.Correction": {
            after: components["schemas"]["correction.history.Identity"];
            before: components["schemas"]["correction.history.Identity"];
            /** Format: date-time */
            correctedAt: string;
            id: components["schemas"]["correction.Id"];
        };
        "correction.history.Identity": {
            idNumber: components["schemas"]["person.number.Suffix"];
            name: components["schemas"]["person.Name"];
        };
        "correction.Id": components["schemas"]["person.revision.Id"];
        "affiliation.Current": {
            affiliation: components["schemas"]["affiliation.Affiliation"];
            code: string;
        };
        "affiliation.Affiliation": {
            /** Format: date-time */
            createdAt: string;
            value: components["schemas"]["affiliation.Code"];
        };
        /** @enum {string} */
        "affiliation.Code": "01" | "02" | "03" | "04" | "05" | "06" | "07" | "08" | "09" | "10" | "11" | "12" | "13";
        "affiliation.Input": {
            affiliation: components["schemas"]["affiliation.Code"];
        };
        "affiliation.Receipt": {
            code: string;
        };
        "affiliation.Values": {
            affiliations: components["schemas"]["affiliation.NamedCode"][];
            code: string;
        };
        "affiliation.NamedCode": {
            name: string;
            value: components["schemas"]["affiliation.Code"];
        };
        "grant.Current": {
            code: string;
            grant: components["schemas"]["grant.Grant"];
        };
        "grant.Grant": {
            /** Format: date-time */
            grantedAt: string;
            id: components["schemas"]["grant.Id"];
            operationId: components["schemas"]["operation.Id"];
            operator: components["schemas"]["person.revision.Identity"];
            /** Format: int64 */
            sequence: number;
            status: components["schemas"]["grant.Status"];
            subject: components["schemas"]["person.revision.Identity"];
        };
        /** Format: uuid */
        "grant.Id": string;
        "person.revision.Identity": {
            name: string;
            personId: components["schemas"]["person.Id"];
            revisionId: components["schemas"]["person.revision.Id"];
        };
        /** @enum {string} */
        "grant.Status": "active" | "suspended" | "terminated";
        "grant.Search": {
            entry: components["schemas"]["access.Entry"];
            mobile: string;
        };
        /** @enum {string} */
        "access.Entry": "domain" | "institution";
        "grant.Candidate": {
            assignmentId: components["schemas"]["appointment.Id"];
            code: string;
            /** Format: int64 */
            count: number;
            grant: components["schemas"]["grant.Confirmation"] | null;
            identity: components["schemas"]["account.Subject"];
            roles: components["schemas"]["role.Role"][];
            suspensionId: components["schemas"]["suspension.Id"] | null;
        };
        /** Format: uuid */
        "appointment.Id": string;
        "grant.Confirmation": {
            id: components["schemas"]["grant.Id"];
            status: components["schemas"]["grant.Status"];
        };
        "account.Subject": {
            accountId: components["schemas"]["account.Id"];
            idNumber: components["schemas"]["person.number.Suffix"];
            name: string;
            personId: components["schemas"]["person.Id"];
            revisionId: components["schemas"]["person.revision.Id"];
        };
        /** Format: uuid */
        "suspension.Id": string;
        "grant.Input": {
            entry: components["schemas"]["access.Entry"];
            role: components["schemas"]["role.Selection"];
            target: components["schemas"]["grant.Target"];
        };
        "role.Selection": {
            changeId: components["schemas"]["role.change.Id"];
            id: components["schemas"]["role.Id"];
        };
        "grant.Target": {
            accountId: components["schemas"]["account.Id"];
            assignmentId: components["schemas"]["appointment.Id"];
            grant?: components["schemas"]["grant.Confirmation"] | null;
            mobile: string;
            personId: components["schemas"]["person.Id"];
            revisionId: components["schemas"]["person.revision.Id"];
            suspensionId?: components["schemas"]["suspension.Id"] | null;
        };
        "grant.Receipt": {
            assignment: components["schemas"]["assignment.Assignment"];
            code: string;
            grant: components["schemas"]["grant.Grant"];
        };
        "assignment.Assignment": {
            /** Format: date-time */
            assignedAt: string;
            grantId: components["schemas"]["grant.Id"];
            id: components["schemas"]["assignment.Id"];
            operationId: components["schemas"]["operation.Id"];
            operator: components["schemas"]["person.revision.Identity"];
            role: components["schemas"]["role.Role"];
            /** Format: int64 */
            sequence: number;
            subject: components["schemas"]["person.revision.Identity"];
        };
        /** Format: uuid */
        "assignment.Id": string;
        /** @description The current number, or null when the person holds none. */
        "number.Current": {
            code: string;
            number: components["schemas"]["number.Revision"] | null;
        };
        "number.Revision": {
            id: components["schemas"]["number.Id"];
            /** Format: int32 */
            serial: number;
            subtype: components["schemas"]["number.Subtype"];
            /** Format: int32 */
            year: number;
        };
        /**
         * Format: uuid
         * @description A fixed number revision; later corrections create new revisions.
         */
        "number.Id": string;
        /** @enum {integer} */
        "number.Subtype": 100 | 200 | 300 | 400 | 500 | 600 | 700 | 801 | 802 | 803 | 804 | 901 | 902 | 903 | 904 | 1000;
        "number.Input": {
            subtype: components["schemas"]["number.Subtype"];
        };
        "number.Receipt": {
            code: string;
            number: components["schemas"]["number.Revision"];
        };
        "termination.Input": {
            target: components["schemas"]["grant.member.Target"];
        };
        "grant.member.Target": {
            accountId: components["schemas"]["account.Id"];
            mobile: string;
            personId: components["schemas"]["person.Id"];
            revisionId: components["schemas"]["person.revision.Id"];
        };
        "termination.Receipt": {
            code: string;
            termination: components["schemas"]["termination.Termination"];
        };
        "termination.Termination": {
            id: components["schemas"]["grant.Id"];
            mode: components["schemas"]["termination.Mode"];
            operationId: components["schemas"]["operation.Id"];
            operator: components["schemas"]["person.revision.Identity"];
            /** Format: int64 */
            sequence: number;
            subject: components["schemas"]["person.revision.Identity"];
            /** Format: date-time */
            terminatedAt: string;
        };
        /** @enum {string} */
        "termination.Mode": "self" | "manager" | "cancellation";
        "revocation.Collection": {
            code: string;
            count: number;
            original: components["schemas"]["revocation.Original"] | null;
            revocations: components["schemas"]["revocation.Revocation"][];
        };
        "revocation.Original": {
            initiatorId: components["schemas"]["session.Id"];
            scope: components["schemas"]["revocation.Scope"];
        };
        /** @enum {string} */
        "revocation.Scope": "current" | "all" | "other";
        "revocation.Revocation": {
            id: components["schemas"]["session.Id"];
            /** Format: date-time */
            revokedAt: string;
        };
        "transfer.Input": {
            code: string;
            messageId: components["schemas"]["message.Id"];
        };
        "transfer.Receipt": {
            code: string;
            transfer: components["schemas"]["transfer.Transfer"];
        };
        "transfer.Transfer": {
            /** Format: date-time */
            expiresAt: string;
            id: components["schemas"]["transfer.Id"];
        };
        /** Format: uuid */
        "transfer.Id": string;
        "transfer.CompletionInput": {
            code: string;
            messageId: components["schemas"]["message.Id"];
            mobile: string;
        };
        "transfer.Completed": {
            code: string;
            completion: components["schemas"]["transfer.Completion"];
        };
        "transfer.Completion": {
            /** Format: date-time */
            completedAt: string;
            id: components["schemas"]["transfer.Id"];
        };
        "transfer.Status": {
            code: string;
            transfer: components["schemas"]["transfer.State"];
        };
        "transfer.State": {
            /** Format: date-time */
            expiresAt: string;
            /** @enum {string} */
            status: "pending";
        } | {
            /** Format: date-time */
            completedAt: string;
            /** @enum {string} */
            status: "completed";
        } | {
            /** @enum {string} */
            status: "expired";
        };
    };
    responses: never;
    parameters: never;
    requestBodies: never;
    headers: never;
    pathItems: never;
}
export type $defs = Record<string, never>;
export interface operations {
    sendCode: {
        parameters: {
            query?: never;
            header: {
                /** @description Required UUID value. Reuse it with the original input while the original records are retained. */
                "Operation-Id": string;
                /** @description Required; must equal the configured HTTPS origin. */
                Origin: string;
            };
            path?: never;
            cookie?: never;
        };
        requestBody: {
            content: {
                "application/json": components["schemas"]["Input"];
            };
        };
        responses: {
            /** @description Original message. sent means supplier acceptance, not delivery. */
            200: {
                headers: {
                    /** @description no-store */
                    "Cache-Control"?: string;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Receipt"];
                };
            };
            /** @description invalid_input; invalid headers or JSON syntax */
            400: {
                headers: {
                    /** @description no-store */
                    "Cache-Control"?: string;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Failure"];
                };
            };
            /** @description authentication_invalid */
            401: {
                headers: {
                    /** @description no-store */
                    "Cache-Control"?: string;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Failure"];
                };
            };
            /** @description verification_invalid or permission_denied */
            403: {
                headers: {
                    /** @description no-store */
                    "Cache-Control"?: string;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Failure"];
                };
            };
            /** @description request_mismatch */
            409: {
                headers: {
                    /** @description no-store */
                    "Cache-Control"?: string;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Failure"];
                };
            };
            /** @description verification_required */
            410: {
                headers: {
                    /** @description no-store */
                    "Cache-Control"?: string;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Failure"];
                };
            };
            /** @description invalid_input; body exceeds 8192 bytes */
            413: {
                headers: {
                    /** @description no-store */
                    "Cache-Control"?: string;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Failure"];
                };
            };
            /** @description invalid_input; JSON media type required */
            415: {
                headers: {
                    /** @description no-store */
                    "Cache-Control"?: string;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Failure"];
                };
            };
            /** @description invalid_input; invalid body shape or field value */
            422: {
                headers: {
                    /** @description no-store */
                    "Cache-Control"?: string;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Failure"];
                };
            };
            /** @description rate_limited; no automatic resend */
            429: {
                headers: {
                    /** @description no-store */
                    "Cache-Control"?: string;
                    /** @description Whole seconds, rounded up, at least one */
                    "Retry-After"?: number;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Failure"];
                };
            };
            /** @description internal_error */
            500: {
                headers: {
                    /** @description no-store */
                    "Cache-Control"?: string;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Failure"];
                };
            };
            /** @description dependency_unavailable or outcome_unknown; retain original input */
            503: {
                headers: {
                    /** @description no-store */
                    "Cache-Control"?: string;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Failure"];
                };
            };
        };
    };
    readPublicSettings: {
        parameters: {
            query?: never;
            header: {
                /** @description Required UUID value for this read. */
                "Operation-Id": string;
            };
            path?: never;
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            /** @description Public widget configuration */
            200: {
                headers: {
                    /** @description no-store */
                    "Cache-Control"?: string;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["PublicSettingsResponse"];
                };
            };
            /** @description invalid_input; invalid operation header */
            400: {
                headers: {
                    /** @description no-store */
                    "Cache-Control"?: string;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Failure"];
                };
            };
        };
    };
    createSession: {
        parameters: {
            query?: never;
            header: {
                /** @description Original R1 operation UUID */
                "Operation-Id": string;
            };
            path?: never;
            cookie?: never;
        };
        requestBody: {
            content: {
                "application/json": components["schemas"]["session.Input"];
            };
        };
        responses: {
            /** @description Source-specific original session or pending registration */
            200: {
                headers: {
                    /** @description no-store */
                    "Cache-Control"?: string;
                    /** @description Only a website session result sets its original secure cookie */
                    "Set-Cookie"?: string;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["session.Receipt"];
                };
            };
            /** @description invalid_input */
            400: {
                headers: {
                    /** @description no-store */
                    "Cache-Control"?: string;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Failure"];
                };
            };
            /** @description authentication_invalid */
            401: {
                headers: {
                    /** @description no-store */
                    "Cache-Control"?: string;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Failure"];
                };
            };
            /** @description verification_invalid, permission_denied or mobile_unsupported */
            403: {
                headers: {
                    /** @description no-store */
                    "Cache-Control"?: string;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Failure"];
                };
            };
            /** @description request_mismatch */
            409: {
                headers: {
                    /** @description no-store */
                    "Cache-Control"?: string;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Failure"];
                };
            };
            /** @description verification_required */
            410: {
                headers: {
                    /** @description no-store */
                    "Cache-Control"?: string;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Failure"];
                };
            };
            /** @description invalid_input */
            413: {
                headers: {
                    /** @description no-store */
                    "Cache-Control"?: string;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Failure"];
                };
            };
            /** @description invalid_input */
            415: {
                headers: {
                    /** @description no-store */
                    "Cache-Control"?: string;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Failure"];
                };
            };
            /** @description invalid_input */
            422: {
                headers: {
                    /** @description no-store */
                    "Cache-Control"?: string;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Failure"];
                };
            };
            /** @description rate_limited */
            429: {
                headers: {
                    /** @description no-store */
                    "Cache-Control"?: string;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Failure"];
                };
            };
            /** @description internal_error */
            500: {
                headers: {
                    /** @description no-store */
                    "Cache-Control"?: string;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Failure"];
                };
            };
            /** @description dependency_unavailable or outcome_unknown */
            503: {
                headers: {
                    /** @description no-store */
                    "Cache-Control"?: string;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Failure"];
                };
            };
        };
    };
    revokeAllSessions: {
        parameters: {
            query?: never;
            header: {
                /** @description Original logout operation UUID */
                "Operation-Id": string;
                /** @description Website cookie mode only */
                "Session-Id"?: string | null;
                /** @description Opaque mini-program Bearer token; exclusive with website credentials */
                Authorization?: string | null;
            };
            path?: never;
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            /** @description All retained unrevoked account sessions revoked */
            200: {
                headers: {
                    /** @description no-store */
                    "Cache-Control"?: string;
                    /** @description Expires only the selected session cookie */
                    "Set-Cookie"?: string;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["revocation.Receipt"];
                };
            };
            /** @description invalid_input; no request body is accepted */
            400: {
                headers: {
                    /** @description no-store */
                    "Cache-Control"?: string;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Failure"];
                };
            };
            /** @description authentication_invalid */
            401: {
                headers: {
                    /** @description no-store */
                    "Cache-Control"?: string;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Failure"];
                };
            };
            /** @description permission_denied */
            403: {
                headers: {
                    /** @description no-store */
                    "Cache-Control"?: string;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Failure"];
                };
            };
            /** @description request_mismatch */
            409: {
                headers: {
                    /** @description no-store */
                    "Cache-Control"?: string;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Failure"];
                };
            };
            /** @description invalid_input */
            413: {
                headers: {
                    /** @description no-store */
                    "Cache-Control"?: string;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Failure"];
                };
            };
            /** @description rate_limited */
            429: {
                headers: {
                    /** @description no-store */
                    "Cache-Control"?: string;
                    /** @description Wait seconds before another attempt */
                    "Retry-After"?: number;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Failure"];
                };
            };
            /** @description internal_error */
            500: {
                headers: {
                    /** @description no-store */
                    "Cache-Control"?: string;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Failure"];
                };
            };
            /** @description dependency_unavailable or outcome_unknown */
            503: {
                headers: {
                    /** @description no-store */
                    "Cache-Control"?: string;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Failure"];
                };
            };
        };
    };
    register: {
        parameters: {
            query?: never;
            header: {
                /** @description Original R2 operation UUID */
                "Operation-Id": string;
            };
            path?: never;
            cookie?: never;
        };
        requestBody: {
            content: {
                "application/json": components["schemas"]["registration.Input"];
            };
        };
        responses: {
            /** @description Original successful registration session */
            200: {
                headers: {
                    /** @description no-store */
                    "Cache-Control"?: string;
                    /** @description Website mode only: the same original secure session cookie */
                    "Set-Cookie"?: string;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["registration.Receipt"];
                };
            };
            /** @description Account created; source-specific original session */
            201: {
                headers: {
                    /** @description no-store */
                    "Cache-Control"?: string;
                    /** @description Website mode only: original secure session cookie */
                    "Set-Cookie"?: string;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["registration.Receipt"];
                };
            };
            /** @description invalid_input */
            400: {
                headers: {
                    /** @description no-store */
                    "Cache-Control"?: string;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Failure"];
                };
            };
            /** @description authentication_invalid */
            401: {
                headers: {
                    /** @description no-store */
                    "Cache-Control"?: string;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Failure"];
                };
            };
            /** @description permission_denied */
            403: {
                headers: {
                    /** @description no-store */
                    "Cache-Control"?: string;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Failure"];
                };
            };
            /** @description request_mismatch, mobile_taken or id_number_taken */
            409: {
                headers: {
                    /** @description no-store */
                    "Cache-Control"?: string;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Failure"];
                };
            };
            /** @description verification_required */
            410: {
                headers: {
                    /** @description no-store */
                    "Cache-Control"?: string;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Failure"];
                };
            };
            /** @description invalid_input */
            413: {
                headers: {
                    /** @description no-store */
                    "Cache-Control"?: string;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Failure"];
                };
            };
            /** @description invalid_input */
            415: {
                headers: {
                    /** @description no-store */
                    "Cache-Control"?: string;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Failure"];
                };
            };
            /** @description invalid_input */
            422: {
                headers: {
                    /** @description no-store */
                    "Cache-Control"?: string;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Failure"];
                };
            };
            /** @description rate_limited */
            429: {
                headers: {
                    /** @description no-store */
                    "Cache-Control"?: string;
                    /** @description Whole seconds, rounded up, at least one */
                    "Retry-After"?: number;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Failure"];
                };
            };
            /** @description internal_error */
            500: {
                headers: {
                    /** @description no-store */
                    "Cache-Control"?: string;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Failure"];
                };
            };
            /** @description dependency_unavailable or outcome_unknown */
            503: {
                headers: {
                    /** @description no-store */
                    "Cache-Control"?: string;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Failure"];
                };
            };
        };
    };
    currentAccount: {
        parameters: {
            query?: never;
            header: {
                /** @description Required request UUID; no persistent read operation */
                "Operation-Id": string;
                /** @description Required only for website cookie mode */
                "Session-Id"?: string | null;
                /** @description Bearer plus a canonical 32-byte opaque token; mini-program mode only */
                Authorization?: string | null;
            };
            path?: never;
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            /** @description Complete current account; no session refresh */
            200: {
                headers: {
                    /** @description no-store */
                    "Cache-Control"?: string;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["current.Receipt"];
                };
            };
            /** @description invalid_input */
            400: {
                headers: {
                    /** @description no-store */
                    "Cache-Control"?: string;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Failure"];
                };
            };
            /** @description authentication_invalid */
            401: {
                headers: {
                    /** @description no-store */
                    "Cache-Control"?: string;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Failure"];
                };
            };
            /** @description permission_denied */
            403: {
                headers: {
                    /** @description no-store */
                    "Cache-Control"?: string;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Failure"];
                };
            };
            /** @description rate_limited */
            429: {
                headers: {
                    /** @description no-store */
                    "Cache-Control"?: string;
                    /** @description Wait seconds before another attempt */
                    "Retry-After"?: number;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Failure"];
                };
            };
            /** @description dependency_unavailable */
            503: {
                headers: {
                    /** @description no-store */
                    "Cache-Control"?: string;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Failure"];
                };
            };
        };
    };
    currentRole: {
        parameters: {
            query?: never;
            header: {
                /** @description Required request UUID; no persistent read operation */
                "Operation-Id": string;
                /** @description Required website session UUID; selects its canonical cookie name; Authorization is rejected */
                "Session-Id": string;
            };
            path?: never;
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            /** @description Complete current institutional role */
            200: {
                headers: {
                    /** @description no-store */
                    "Cache-Control"?: string;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["role.Receipt"];
                };
            };
            /** @description invalid_input */
            400: {
                headers: {
                    /** @description no-store */
                    "Cache-Control"?: string;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Failure"];
                };
            };
            /** @description authentication_invalid */
            401: {
                headers: {
                    /** @description no-store */
                    "Cache-Control"?: string;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Failure"];
                };
            };
            /** @description permission_denied */
            403: {
                headers: {
                    /** @description no-store */
                    "Cache-Control"?: string;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Failure"];
                };
            };
            /** @description rate_limited */
            429: {
                headers: {
                    /** @description no-store */
                    "Cache-Control"?: string;
                    /** @description Wait seconds before another attempt */
                    "Retry-After"?: number;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Failure"];
                };
            };
            /** @description dependency_unavailable */
            503: {
                headers: {
                    /** @description no-store */
                    "Cache-Control"?: string;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Failure"];
                };
            };
        };
    };
    cancelCurrentAccount: {
        parameters: {
            query?: never;
            header: {
                /** @description Cancellation operation UUID */
                "Operation-Id": string;
                /** @description Required website session UUID */
                "Session-Id": string;
                /** @description Required exact configured origin */
                Origin: string;
            };
            path?: never;
            cookie?: never;
        };
        requestBody: {
            content: {
                "application/json": components["schemas"]["cancellation.Input"];
            };
        };
        responses: {
            /** @description Permanent cancellation ID and effective time */
            200: {
                headers: {
                    /** @description no-store */
                    "Cache-Control"?: string;
                    /** @description Clears only the authenticated session cookie */
                    "Set-Cookie"?: string;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["cancellation.Receipt"];
                };
            };
            /** @description invalid_input */
            400: {
                headers: {
                    /** @description no-store */
                    "Cache-Control"?: string;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Failure"];
                };
            };
            /** @description authentication_invalid */
            401: {
                headers: {
                    /** @description no-store */
                    "Cache-Control"?: string;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Failure"];
                };
            };
            /** @description permission_denied */
            403: {
                headers: {
                    /** @description no-store */
                    "Cache-Control"?: string;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Failure"];
                };
            };
            /** @description request_mismatch or target_changed */
            409: {
                headers: {
                    /** @description no-store */
                    "Cache-Control"?: string;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Failure"];
                };
            };
            /** @description invalid_input; body exceeds 8 KiB */
            413: {
                headers: {
                    /** @description no-store */
                    "Cache-Control"?: string;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Failure"];
                };
            };
            /** @description invalid_input */
            415: {
                headers: {
                    /** @description no-store */
                    "Cache-Control"?: string;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Failure"];
                };
            };
            /** @description invalid_input */
            422: {
                headers: {
                    /** @description no-store */
                    "Cache-Control"?: string;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Failure"];
                };
            };
            /** @description rate_limited */
            429: {
                headers: {
                    /** @description no-store */
                    "Cache-Control"?: string;
                    /** @description Wait seconds */
                    "Retry-After"?: number;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Failure"];
                };
            };
            /** @description internal_error */
            500: {
                headers: {
                    /** @description no-store */
                    "Cache-Control"?: string;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Failure"];
                };
            };
            /** @description dependency_unavailable or outcome_unknown */
            503: {
                headers: {
                    /** @description no-store */
                    "Cache-Control"?: string;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Failure"];
                };
            };
        };
    };
    listCurrentCorrections: {
        parameters: {
            query?: {
                /** @description Defaults to 1 */
                page?: number;
                /** @description Defaults to 20 */
                size?: number;
            };
            header: {
                /** @description Required request UUID; no persistent read operation */
                "Operation-Id": string;
                /** @description Required website session UUID */
                "Session-Id": string;
            };
            path?: never;
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            /** @description Complete self-visible correction history page */
            200: {
                headers: {
                    /** @description no-store */
                    "Cache-Control"?: string;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["correction.History"];
                };
            };
            /** @description invalid_input */
            400: {
                headers: {
                    /** @description no-store */
                    "Cache-Control"?: string;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Failure"];
                };
            };
            /** @description authentication_invalid */
            401: {
                headers: {
                    /** @description no-store */
                    "Cache-Control"?: string;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Failure"];
                };
            };
            /** @description permission_denied */
            403: {
                headers: {
                    /** @description no-store */
                    "Cache-Control"?: string;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Failure"];
                };
            };
            /** @description rate_limited */
            429: {
                headers: {
                    /** @description no-store */
                    "Cache-Control"?: string;
                    /** @description Wait seconds */
                    "Retry-After"?: number;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Failure"];
                };
            };
            /** @description dependency_unavailable */
            503: {
                headers: {
                    /** @description no-store */
                    "Cache-Control"?: string;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Failure"];
                };
            };
        };
    };
    currentAffiliation: {
        parameters: {
            query?: never;
            header: {
                /** @description Required request UUID; no persistent read operation */
                "Operation-Id": string;
                /** @description Required website session UUID selecting its cookie */
                "Session-Id": string;
            };
            path?: never;
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            /** @description Current affiliation value and effective time without operator identity */
            200: {
                headers: {
                    /** @description no-store */
                    "Cache-Control"?: string;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["affiliation.Current"];
                };
            };
            /** @description invalid_input */
            400: {
                headers: {
                    /** @description no-store */
                    "Cache-Control"?: string;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Failure"];
                };
            };
            /** @description authentication_invalid */
            401: {
                headers: {
                    /** @description no-store */
                    "Cache-Control"?: string;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Failure"];
                };
            };
            /** @description permission_denied */
            403: {
                headers: {
                    /** @description no-store */
                    "Cache-Control"?: string;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Failure"];
                };
            };
            /** @description affiliation_unset */
            404: {
                headers: {
                    /** @description no-store */
                    "Cache-Control"?: string;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Failure"];
                };
            };
            /** @description invalid_input; body exceeds 8 KiB */
            413: {
                headers: {
                    /** @description no-store */
                    "Cache-Control"?: string;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Failure"];
                };
            };
            /** @description rate_limited */
            429: {
                headers: {
                    /** @description no-store */
                    "Cache-Control"?: string;
                    /** @description Wait seconds */
                    "Retry-After"?: number;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Failure"];
                };
            };
            /** @description internal_error */
            500: {
                headers: {
                    /** @description no-store */
                    "Cache-Control"?: string;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Failure"];
                };
            };
            /** @description dependency_unavailable */
            503: {
                headers: {
                    /** @description no-store */
                    "Cache-Control"?: string;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Failure"];
                };
            };
        };
    };
    createCurrentAffiliation: {
        parameters: {
            query?: never;
            header: {
                /** @description Required operation UUID */
                "Operation-Id": string;
                /** @description Required website session UUID */
                "Session-Id": string;
                /** @description Required exact configured origin */
                Origin: string;
            };
            path?: never;
            cookie?: never;
        };
        requestBody: {
            content: {
                "application/json": components["schemas"]["affiliation.Input"];
            };
        };
        responses: {
            /** @description First affiliation recorded */
            200: {
                headers: {
                    /** @description no-store */
                    "Cache-Control"?: string;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["affiliation.Receipt"];
                };
            };
            /** @description invalid_input */
            400: {
                headers: {
                    /** @description no-store */
                    "Cache-Control"?: string;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Failure"];
                };
            };
            /** @description authentication_invalid */
            401: {
                headers: {
                    /** @description no-store */
                    "Cache-Control"?: string;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Failure"];
                };
            };
            /** @description permission_denied */
            403: {
                headers: {
                    /** @description no-store */
                    "Cache-Control"?: string;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Failure"];
                };
            };
            /** @description state_conflict */
            409: {
                headers: {
                    /** @description no-store */
                    "Cache-Control"?: string;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Failure"];
                };
            };
            /** @description invalid_input; body exceeds 8 KiB */
            413: {
                headers: {
                    /** @description no-store */
                    "Cache-Control"?: string;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Failure"];
                };
            };
            /** @description invalid_input */
            415: {
                headers: {
                    /** @description no-store */
                    "Cache-Control"?: string;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Failure"];
                };
            };
            /** @description invalid_input */
            422: {
                headers: {
                    /** @description no-store */
                    "Cache-Control"?: string;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Failure"];
                };
            };
            /** @description rate_limited */
            429: {
                headers: {
                    /** @description no-store */
                    "Cache-Control"?: string;
                    /** @description Wait seconds */
                    "Retry-After"?: number;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Failure"];
                };
            };
            /** @description internal_error */
            500: {
                headers: {
                    /** @description no-store */
                    "Cache-Control"?: string;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Failure"];
                };
            };
            /** @description dependency_unavailable or outcome_unknown */
            503: {
                headers: {
                    /** @description no-store */
                    "Cache-Control"?: string;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Failure"];
                };
            };
        };
    };
    affiliationValues: {
        parameters: {
            query?: never;
            header: {
                /** @description Required request UUID; no persistent read operation */
                "Operation-Id": string;
                /** @description Required website session UUID selecting its cookie */
                "Session-Id": string;
            };
            path?: never;
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            /** @description Complete fixed code and display-name vocabulary; no default or unset entry */
            200: {
                headers: {
                    /** @description no-store */
                    "Cache-Control"?: string;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["affiliation.Values"];
                };
            };
            /** @description invalid_input */
            400: {
                headers: {
                    /** @description no-store */
                    "Cache-Control"?: string;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Failure"];
                };
            };
            /** @description authentication_invalid */
            401: {
                headers: {
                    /** @description no-store */
                    "Cache-Control"?: string;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Failure"];
                };
            };
            /** @description permission_denied */
            403: {
                headers: {
                    /** @description no-store */
                    "Cache-Control"?: string;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Failure"];
                };
            };
            /** @description invalid_input; body exceeds 8 KiB */
            413: {
                headers: {
                    /** @description no-store */
                    "Cache-Control"?: string;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Failure"];
                };
            };
            /** @description rate_limited */
            429: {
                headers: {
                    /** @description no-store */
                    "Cache-Control"?: string;
                    /** @description Wait seconds */
                    "Retry-After"?: number;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Failure"];
                };
            };
            /** @description internal_error */
            500: {
                headers: {
                    /** @description no-store */
                    "Cache-Control"?: string;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Failure"];
                };
            };
            /** @description dependency_unavailable */
            503: {
                headers: {
                    /** @description no-store */
                    "Cache-Control"?: string;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Failure"];
                };
            };
        };
    };
    currentWorkforceGrant: {
        parameters: {
            query?: never;
            header: {
                /** @description This read request UUID */
                "Operation-Id": string;
                /** @description Selects a currently valid website session cookie */
                "Session-Id": string;
            };
            path?: never;
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            /** @description Latest grant with fixed masked identities and current status */
            200: {
                headers: {
                    /** @description no-store */
                    "Cache-Control"?: string;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["grant.Current"];
                };
            };
            /** @description invalid_input */
            400: {
                headers: {
                    /** @description no-store */
                    "Cache-Control"?: string;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Failure"];
                };
            };
            /** @description authentication_invalid */
            401: {
                headers: {
                    /** @description no-store */
                    "Cache-Control"?: string;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Failure"];
                };
            };
            /** @description permission_denied */
            403: {
                headers: {
                    /** @description no-store */
                    "Cache-Control"?: string;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Failure"];
                };
            };
            /** @description grant_absent */
            404: {
                headers: {
                    /** @description no-store */
                    "Cache-Control"?: string;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Failure"];
                };
            };
            /** @description invalid_input */
            413: {
                headers: {
                    /** @description no-store */
                    "Cache-Control"?: string;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Failure"];
                };
            };
            /** @description rate_limited */
            429: {
                headers: {
                    /** @description no-store */
                    "Cache-Control"?: string;
                    /** @description Wait seconds */
                    "Retry-After"?: number;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Failure"];
                };
            };
            /** @description internal_error */
            500: {
                headers: {
                    /** @description no-store */
                    "Cache-Control"?: string;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Failure"];
                };
            };
            /** @description dependency_unavailable */
            503: {
                headers: {
                    /** @description no-store */
                    "Cache-Control"?: string;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Failure"];
                };
            };
        };
    };
    workforceGrantCandidate: {
        parameters: {
            query?: {
                /** @description Defaults to 1; (page - 1) * size must fit PostgreSQL bigint */
                page?: number;
                /** @description Defaults to 20 */
                size?: number;
            };
            header: {
                /** @description Required request UUID */
                "Operation-Id": string;
                /** @description Required website session UUID */
                "Session-Id": string;
                /** @description Required exact configured origin */
                Origin: string;
            };
            path?: never;
            cookie?: never;
        };
        requestBody: {
            content: {
                "application/json": components["schemas"]["grant.Search"];
            };
        };
        responses: {
            /** @description Masked candidate and role choices */
            200: {
                headers: {
                    /** @description no-store */
                    "Cache-Control"?: string;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["grant.Candidate"];
                };
            };
            /** @description invalid_input */
            400: {
                headers: {
                    /** @description no-store */
                    "Cache-Control"?: string;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Failure"];
                };
            };
            /** @description authentication_invalid */
            401: {
                headers: {
                    /** @description no-store */
                    "Cache-Control"?: string;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Failure"];
                };
            };
            /** @description permission_denied */
            403: {
                headers: {
                    /** @description no-store */
                    "Cache-Control"?: string;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Failure"];
                };
            };
            /** @description target_unavailable */
            404: {
                headers: {
                    /** @description no-store */
                    "Cache-Control"?: string;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Failure"];
                };
            };
            /** @description invalid_input */
            413: {
                headers: {
                    /** @description no-store */
                    "Cache-Control"?: string;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Failure"];
                };
            };
            /** @description invalid_input */
            415: {
                headers: {
                    /** @description no-store */
                    "Cache-Control"?: string;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Failure"];
                };
            };
            /** @description invalid_input */
            422: {
                headers: {
                    /** @description no-store */
                    "Cache-Control"?: string;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Failure"];
                };
            };
            /** @description rate_limited */
            429: {
                headers: {
                    /** @description no-store */
                    "Cache-Control"?: string;
                    /** @description Wait seconds */
                    "Retry-After"?: number;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Failure"];
                };
            };
            /** @description dependency_unavailable */
            503: {
                headers: {
                    /** @description no-store */
                    "Cache-Control"?: string;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Failure"];
                };
            };
        };
    };
    createWorkforceGrant: {
        parameters: {
            query?: never;
            header: {
                /** @description Original admission UUID */
                "Operation-Id": string;
                /** @description Required website session UUID */
                "Session-Id": string;
                /** @description Required exact configured origin */
                Origin: string;
            };
            path?: never;
            cookie?: never;
        };
        requestBody: {
            content: {
                "application/json": components["schemas"]["grant.Input"];
            };
        };
        responses: {
            /** @description Original grant and first assignment */
            201: {
                headers: {
                    /** @description no-store */
                    "Cache-Control"?: string;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["grant.Receipt"];
                };
            };
            /** @description invalid_input */
            400: {
                headers: {
                    /** @description no-store */
                    "Cache-Control"?: string;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Failure"];
                };
            };
            /** @description authentication_invalid */
            401: {
                headers: {
                    /** @description no-store */
                    "Cache-Control"?: string;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Failure"];
                };
            };
            /** @description permission_denied */
            403: {
                headers: {
                    /** @description no-store */
                    "Cache-Control"?: string;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Failure"];
                };
            };
            /** @description target_unavailable */
            404: {
                headers: {
                    /** @description no-store */
                    "Cache-Control"?: string;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Failure"];
                };
            };
            /** @description request_mismatch, target_changed, role_changed or already_authorized */
            409: {
                headers: {
                    /** @description no-store */
                    "Cache-Control"?: string;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Failure"];
                };
            };
            /** @description invalid_input */
            413: {
                headers: {
                    /** @description no-store */
                    "Cache-Control"?: string;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Failure"];
                };
            };
            /** @description invalid_input */
            415: {
                headers: {
                    /** @description no-store */
                    "Cache-Control"?: string;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Failure"];
                };
            };
            /** @description invalid_input or account_not_ready */
            422: {
                headers: {
                    /** @description no-store */
                    "Cache-Control"?: string;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Failure"];
                };
            };
            /** @description rate_limited */
            429: {
                headers: {
                    /** @description no-store */
                    "Cache-Control"?: string;
                    /** @description Wait seconds */
                    "Retry-After"?: number;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Failure"];
                };
            };
            /** @description internal_error */
            500: {
                headers: {
                    /** @description no-store */
                    "Cache-Control"?: string;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Failure"];
                };
            };
            /** @description dependency_unavailable or outcome_unknown */
            503: {
                headers: {
                    /** @description no-store */
                    "Cache-Control"?: string;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Failure"];
                };
            };
        };
    };
    currentWorkforceNumber: {
        parameters: {
            query?: never;
            header: {
                /** @description This read request UUID */
                "Operation-Id": string;
                /** @description Selects a currently valid website session cookie */
                "Session-Id": string;
            };
            path?: never;
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            /** @description Current fixed revision with its complete number, or null */
            200: {
                headers: {
                    /** @description no-store */
                    "Cache-Control"?: string;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["number.Current"];
                };
            };
            /** @description invalid_input */
            400: {
                headers: {
                    /** @description no-store */
                    "Cache-Control"?: string;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Failure"];
                };
            };
            /** @description authentication_invalid */
            401: {
                headers: {
                    /** @description no-store */
                    "Cache-Control"?: string;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Failure"];
                };
            };
            /** @description permission_denied */
            403: {
                headers: {
                    /** @description no-store */
                    "Cache-Control"?: string;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Failure"];
                };
            };
            /** @description invalid_input */
            413: {
                headers: {
                    /** @description no-store */
                    "Cache-Control"?: string;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Failure"];
                };
            };
            /** @description rate_limited */
            429: {
                headers: {
                    /** @description no-store */
                    "Cache-Control"?: string;
                    /** @description Wait seconds */
                    "Retry-After"?: number;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Failure"];
                };
            };
            /** @description internal_error */
            500: {
                headers: {
                    /** @description no-store */
                    "Cache-Control"?: string;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Failure"];
                };
            };
            /** @description dependency_unavailable */
            503: {
                headers: {
                    /** @description no-store */
                    "Cache-Control"?: string;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Failure"];
                };
            };
        };
    };
    allocateWorkforceNumber: {
        parameters: {
            query?: never;
            header: {
                /** @description Original allocation UUID */
                "Operation-Id": string;
                /** @description Required website session UUID */
                "Session-Id": string;
                /** @description Required exact configured origin */
                Origin: string;
            };
            path?: never;
            cookie?: never;
        };
        requestBody: {
            content: {
                "application/json": components["schemas"]["number.Input"];
            };
        };
        responses: {
            /** @description Original fixed revision with its complete number */
            201: {
                headers: {
                    /** @description no-store */
                    "Cache-Control"?: string;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["number.Receipt"];
                };
            };
            /** @description invalid_input */
            400: {
                headers: {
                    /** @description no-store */
                    "Cache-Control"?: string;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Failure"];
                };
            };
            /** @description authentication_invalid */
            401: {
                headers: {
                    /** @description no-store */
                    "Cache-Control"?: string;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Failure"];
                };
            };
            /** @description permission_denied */
            403: {
                headers: {
                    /** @description no-store */
                    "Cache-Control"?: string;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Failure"];
                };
            };
            /** @description number_present, number_pool_exhausted or request_mismatch */
            409: {
                headers: {
                    /** @description no-store */
                    "Cache-Control"?: string;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Failure"];
                };
            };
            /** @description invalid_input; body exceeds 8 KiB */
            413: {
                headers: {
                    /** @description no-store */
                    "Cache-Control"?: string;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Failure"];
                };
            };
            /** @description invalid_input */
            415: {
                headers: {
                    /** @description no-store */
                    "Cache-Control"?: string;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Failure"];
                };
            };
            /** @description invalid_input */
            422: {
                headers: {
                    /** @description no-store */
                    "Cache-Control"?: string;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Failure"];
                };
            };
            /** @description rate_limited */
            429: {
                headers: {
                    /** @description no-store */
                    "Cache-Control"?: string;
                    /** @description Wait seconds */
                    "Retry-After"?: number;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Failure"];
                };
            };
            /** @description internal_error */
            500: {
                headers: {
                    /** @description no-store */
                    "Cache-Control"?: string;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Failure"];
                };
            };
            /** @description dependency_unavailable or outcome_unknown */
            503: {
                headers: {
                    /** @description no-store */
                    "Cache-Control"?: string;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Failure"];
                };
            };
        };
    };
    terminateWorkforceGrant: {
        parameters: {
            query?: never;
            header: {
                /** @description Original termination UUID */
                "Operation-Id": string;
                /** @description Required website session UUID */
                "Session-Id": string;
                /** @description Required exact configured origin */
                Origin: string;
            };
            path: {
                /** @description The exact original Workforce Grant */
                id: components["schemas"]["grant.Id"];
            };
            cookie?: never;
        };
        /** @description Omit the body for self exit; a nonempty body must contain the complete managed target confirmation. */
        requestBody?: {
            content: {
                "application/json": components["schemas"]["termination.Input"];
            };
        };
        responses: {
            /** @description Original termination with fixed masked identities and self or manager mode */
            201: {
                headers: {
                    /** @description no-store */
                    "Cache-Control"?: string;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["termination.Receipt"];
                };
            };
            /** @description invalid_input */
            400: {
                headers: {
                    /** @description no-store */
                    "Cache-Control"?: string;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Failure"];
                };
            };
            /** @description authentication_invalid */
            401: {
                headers: {
                    /** @description no-store */
                    "Cache-Control"?: string;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Failure"];
                };
            };
            /** @description permission_denied */
            403: {
                headers: {
                    /** @description no-store */
                    "Cache-Control"?: string;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Failure"];
                };
            };
            /** @description target_unavailable */
            404: {
                headers: {
                    /** @description no-store */
                    "Cache-Control"?: string;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Failure"];
                };
            };
            /** @description request_mismatch, target_changed or state_conflict */
            409: {
                headers: {
                    /** @description no-store */
                    "Cache-Control"?: string;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Failure"];
                };
            };
            /** @description invalid_input */
            413: {
                headers: {
                    /** @description no-store */
                    "Cache-Control"?: string;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Failure"];
                };
            };
            /** @description invalid_input */
            415: {
                headers: {
                    /** @description no-store */
                    "Cache-Control"?: string;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Failure"];
                };
            };
            /** @description invalid_input */
            422: {
                headers: {
                    /** @description no-store */
                    "Cache-Control"?: string;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Failure"];
                };
            };
            /** @description rate_limited */
            429: {
                headers: {
                    /** @description no-store */
                    "Cache-Control"?: string;
                    /** @description Wait seconds */
                    "Retry-After"?: number;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Failure"];
                };
            };
            /** @description internal_error */
            500: {
                headers: {
                    /** @description no-store */
                    "Cache-Control"?: string;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Failure"];
                };
            };
            /** @description dependency_unavailable or outcome_unknown */
            503: {
                headers: {
                    /** @description no-store */
                    "Cache-Control"?: string;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Failure"];
                };
            };
        };
    };
    revokeCurrentSession: {
        parameters: {
            query?: never;
            header: {
                /** @description Original logout operation UUID */
                "Operation-Id": string;
                /** @description Website cookie mode only */
                "Session-Id"?: string | null;
                /** @description Opaque mini-program Bearer token; exclusive with website credentials */
                Authorization?: string | null;
            };
            path?: never;
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            /** @description Current session revoked */
            200: {
                headers: {
                    /** @description no-store */
                    "Cache-Control"?: string;
                    /** @description Expires only the selected session cookie */
                    "Set-Cookie"?: string;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["revocation.Receipt"];
                };
            };
            /** @description invalid_input; no request body is accepted */
            400: {
                headers: {
                    /** @description no-store */
                    "Cache-Control"?: string;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Failure"];
                };
            };
            /** @description authentication_invalid */
            401: {
                headers: {
                    /** @description no-store */
                    "Cache-Control"?: string;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Failure"];
                };
            };
            /** @description permission_denied */
            403: {
                headers: {
                    /** @description no-store */
                    "Cache-Control"?: string;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Failure"];
                };
            };
            /** @description request_mismatch */
            409: {
                headers: {
                    /** @description no-store */
                    "Cache-Control"?: string;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Failure"];
                };
            };
            /** @description invalid_input */
            413: {
                headers: {
                    /** @description no-store */
                    "Cache-Control"?: string;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Failure"];
                };
            };
            /** @description rate_limited */
            429: {
                headers: {
                    /** @description no-store */
                    "Cache-Control"?: string;
                    /** @description Wait seconds before another attempt */
                    "Retry-After"?: number;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Failure"];
                };
            };
            /** @description internal_error */
            500: {
                headers: {
                    /** @description no-store */
                    "Cache-Control"?: string;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Failure"];
                };
            };
            /** @description dependency_unavailable or outcome_unknown */
            503: {
                headers: {
                    /** @description no-store */
                    "Cache-Control"?: string;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Failure"];
                };
            };
        };
    };
    readRevocations: {
        parameters: {
            query: {
                /** @description Original logout UUID */
                operation_id: string;
            };
            header: {
                /** @description This read request UUID */
                "Operation-Id": string;
                /** @description Website cookie mode only */
                "Session-Id"?: string | null;
                /** @description Opaque mini-program Bearer token; exclusive with website credentials */
                Authorization?: string | null;
            };
            path?: never;
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            /** @description Original account-scoped logout set */
            200: {
                headers: {
                    /** @description no-store */
                    "Cache-Control"?: string;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["revocation.Collection"];
                };
            };
            /** @description invalid_input */
            400: {
                headers: {
                    /** @description no-store */
                    "Cache-Control"?: string;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Failure"];
                };
            };
            /** @description authentication_invalid */
            401: {
                headers: {
                    /** @description no-store */
                    "Cache-Control"?: string;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Failure"];
                };
            };
            /** @description permission_denied */
            403: {
                headers: {
                    /** @description no-store */
                    "Cache-Control"?: string;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Failure"];
                };
            };
            /** @description invalid_input */
            413: {
                headers: {
                    /** @description no-store */
                    "Cache-Control"?: string;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Failure"];
                };
            };
            /** @description rate_limited */
            429: {
                headers: {
                    /** @description no-store */
                    "Cache-Control"?: string;
                    /** @description Wait seconds before another attempt */
                    "Retry-After"?: number;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Failure"];
                };
            };
            /** @description internal_error */
            500: {
                headers: {
                    /** @description no-store */
                    "Cache-Control"?: string;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Failure"];
                };
            };
            /** @description dependency_unavailable */
            503: {
                headers: {
                    /** @description no-store */
                    "Cache-Control"?: string;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Failure"];
                };
            };
        };
    };
    sendCurrentMobileCode: {
        parameters: {
            query?: never;
            header: {
                /** @description Original sending operation UUID */
                "Operation-Id": string;
                /** @description Selects the website session cookie */
                "Session-Id": string;
                /** @description Required exact configured origin */
                Origin: string;
            };
            path?: never;
            cookie?: never;
        };
        requestBody: {
            content: {
                "application/json": components["schemas"]["Input"];
            };
        };
        responses: {
            /** @description Original message; sent means supplier acceptance */
            200: {
                headers: {
                    /** @description no-store */
                    "Cache-Control"?: string;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Receipt"];
                };
            };
            /** @description invalid_input */
            400: {
                headers: {
                    /** @description no-store */
                    "Cache-Control"?: string;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Failure"];
                };
            };
            /** @description authentication_invalid */
            401: {
                headers: {
                    /** @description no-store */
                    "Cache-Control"?: string;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Failure"];
                };
            };
            /** @description verification_invalid or permission_denied */
            403: {
                headers: {
                    /** @description no-store */
                    "Cache-Control"?: string;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Failure"];
                };
            };
            /** @description request_mismatch or target_changed */
            409: {
                headers: {
                    /** @description no-store */
                    "Cache-Control"?: string;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Failure"];
                };
            };
            /** @description verification_required */
            410: {
                headers: {
                    /** @description no-store */
                    "Cache-Control"?: string;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Failure"];
                };
            };
            /** @description invalid_input; body exceeds 8192 bytes */
            413: {
                headers: {
                    /** @description no-store */
                    "Cache-Control"?: string;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Failure"];
                };
            };
            /** @description invalid_input */
            415: {
                headers: {
                    /** @description no-store */
                    "Cache-Control"?: string;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Failure"];
                };
            };
            /** @description invalid_input */
            422: {
                headers: {
                    /** @description no-store */
                    "Cache-Control"?: string;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Failure"];
                };
            };
            /** @description rate_limited */
            429: {
                headers: {
                    /** @description no-store */
                    "Cache-Control"?: string;
                    /** @description Wait seconds */
                    "Retry-After"?: number;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Failure"];
                };
            };
            /** @description internal_error */
            500: {
                headers: {
                    /** @description no-store */
                    "Cache-Control"?: string;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Failure"];
                };
            };
            /** @description dependency_unavailable or outcome_unknown */
            503: {
                headers: {
                    /** @description no-store */
                    "Cache-Control"?: string;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Failure"];
                };
            };
        };
    };
    createMobileTransfer: {
        parameters: {
            query?: never;
            header: {
                /** @description Original transfer operation UUID */
                "Operation-Id": string;
                /** @description Selects the website session cookie */
                "Session-Id": string;
                /** @description Required exact configured origin */
                Origin: string;
            };
            path?: never;
            cookie?: never;
        };
        requestBody: {
            content: {
                "application/json": components["schemas"]["transfer.Input"];
            };
        };
        responses: {
            /** @description Established transfer ID and fixed expiry */
            201: {
                headers: {
                    /** @description no-store */
                    "Cache-Control"?: string;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["transfer.Receipt"];
                };
            };
            /** @description invalid_input */
            400: {
                headers: {
                    /** @description no-store */
                    "Cache-Control"?: string;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Failure"];
                };
            };
            /** @description authentication_invalid */
            401: {
                headers: {
                    /** @description no-store */
                    "Cache-Control"?: string;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Failure"];
                };
            };
            /** @description verification_invalid or permission_denied */
            403: {
                headers: {
                    /** @description no-store */
                    "Cache-Control"?: string;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Failure"];
                };
            };
            /** @description request_mismatch */
            409: {
                headers: {
                    /** @description no-store */
                    "Cache-Control"?: string;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Failure"];
                };
            };
            /** @description verification_required */
            410: {
                headers: {
                    /** @description no-store */
                    "Cache-Control"?: string;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Failure"];
                };
            };
            /** @description invalid_input; body exceeds 8192 bytes */
            413: {
                headers: {
                    /** @description no-store */
                    "Cache-Control"?: string;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Failure"];
                };
            };
            /** @description invalid_input */
            415: {
                headers: {
                    /** @description no-store */
                    "Cache-Control"?: string;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Failure"];
                };
            };
            /** @description invalid_input */
            422: {
                headers: {
                    /** @description no-store */
                    "Cache-Control"?: string;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Failure"];
                };
            };
            /** @description rate_limited */
            429: {
                headers: {
                    /** @description no-store */
                    "Cache-Control"?: string;
                    /** @description Wait seconds */
                    "Retry-After"?: number;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Failure"];
                };
            };
            /** @description internal_error */
            500: {
                headers: {
                    /** @description no-store */
                    "Cache-Control"?: string;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Failure"];
                };
            };
            /** @description dependency_unavailable or outcome_unknown */
            503: {
                headers: {
                    /** @description no-store */
                    "Cache-Control"?: string;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Failure"];
                };
            };
        };
    };
    sendNewMobileCode: {
        parameters: {
            query?: never;
            header: {
                /** @description Original sending operation UUID */
                "Operation-Id": string;
                /** @description Selects the website session cookie */
                "Session-Id": string;
                /** @description Required exact configured origin */
                Origin: string;
            };
            path: {
                /** @description Original mobile transfer */
                id: components["schemas"]["transfer.Id"];
            };
            cookie?: never;
        };
        requestBody: {
            content: {
                "application/json": components["schemas"]["Input"];
            };
        };
        responses: {
            /** @description Original message; sent means supplier acceptance */
            200: {
                headers: {
                    /** @description no-store */
                    "Cache-Control"?: string;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Receipt"];
                };
            };
            /** @description invalid_input */
            400: {
                headers: {
                    /** @description no-store */
                    "Cache-Control"?: string;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Failure"];
                };
            };
            /** @description authentication_invalid */
            401: {
                headers: {
                    /** @description no-store */
                    "Cache-Control"?: string;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Failure"];
                };
            };
            /** @description verification_invalid or permission_denied */
            403: {
                headers: {
                    /** @description no-store */
                    "Cache-Control"?: string;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Failure"];
                };
            };
            /** @description target_unavailable; assisted target is not currently manageable */
            404: {
                headers: {
                    /** @description no-store */
                    "Cache-Control"?: string;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Failure"];
                };
            };
            /** @description request_mismatch or state_conflict */
            409: {
                headers: {
                    /** @description no-store */
                    "Cache-Control"?: string;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Failure"];
                };
            };
            /** @description verification_required; transfer or verification no longer applicable */
            410: {
                headers: {
                    /** @description no-store */
                    "Cache-Control"?: string;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Failure"];
                };
            };
            /** @description invalid_input; body exceeds 8192 bytes */
            413: {
                headers: {
                    /** @description no-store */
                    "Cache-Control"?: string;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Failure"];
                };
            };
            /** @description invalid_input */
            415: {
                headers: {
                    /** @description no-store */
                    "Cache-Control"?: string;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Failure"];
                };
            };
            /** @description invalid_input */
            422: {
                headers: {
                    /** @description no-store */
                    "Cache-Control"?: string;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Failure"];
                };
            };
            /** @description rate_limited */
            429: {
                headers: {
                    /** @description no-store */
                    "Cache-Control"?: string;
                    /** @description Wait seconds */
                    "Retry-After"?: number;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Failure"];
                };
            };
            /** @description internal_error */
            500: {
                headers: {
                    /** @description no-store */
                    "Cache-Control"?: string;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Failure"];
                };
            };
            /** @description dependency_unavailable or outcome_unknown */
            503: {
                headers: {
                    /** @description no-store */
                    "Cache-Control"?: string;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Failure"];
                };
            };
        };
    };
    completeMobileTransfer: {
        parameters: {
            query?: never;
            header: {
                /** @description Original completion operation UUID */
                "Operation-Id": string;
                /** @description Selects the original transfer session cookie */
                "Session-Id": string;
                /** @description Required exact configured origin */
                Origin: string;
            };
            path?: never;
            cookie?: never;
        };
        requestBody: {
            content: {
                "application/json": components["schemas"]["transfer.CompletionInput"];
            };
        };
        responses: {
            /** @description Completed transfer ID and effective time */
            200: {
                headers: {
                    /** @description no-store */
                    "Cache-Control"?: string;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["transfer.Completed"];
                };
            };
            /** @description invalid_input */
            400: {
                headers: {
                    /** @description no-store */
                    "Cache-Control"?: string;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Failure"];
                };
            };
            /** @description authentication_invalid */
            401: {
                headers: {
                    /** @description no-store */
                    "Cache-Control"?: string;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Failure"];
                };
            };
            /** @description verification_invalid or permission_denied */
            403: {
                headers: {
                    /** @description no-store */
                    "Cache-Control"?: string;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Failure"];
                };
            };
            /** @description request_mismatch, state_conflict or mobile_taken */
            409: {
                headers: {
                    /** @description no-store */
                    "Cache-Control"?: string;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Failure"];
                };
            };
            /** @description verification_required */
            410: {
                headers: {
                    /** @description no-store */
                    "Cache-Control"?: string;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Failure"];
                };
            };
            /** @description invalid_input; body exceeds 8192 bytes */
            413: {
                headers: {
                    /** @description no-store */
                    "Cache-Control"?: string;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Failure"];
                };
            };
            /** @description invalid_input */
            415: {
                headers: {
                    /** @description no-store */
                    "Cache-Control"?: string;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Failure"];
                };
            };
            /** @description invalid_input */
            422: {
                headers: {
                    /** @description no-store */
                    "Cache-Control"?: string;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Failure"];
                };
            };
            /** @description rate_limited */
            429: {
                headers: {
                    /** @description no-store */
                    "Cache-Control"?: string;
                    /** @description Wait seconds */
                    "Retry-After"?: number;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Failure"];
                };
            };
            /** @description internal_error */
            500: {
                headers: {
                    /** @description no-store */
                    "Cache-Control"?: string;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Failure"];
                };
            };
            /** @description dependency_unavailable or outcome_unknown */
            503: {
                headers: {
                    /** @description no-store */
                    "Cache-Control"?: string;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Failure"];
                };
            };
        };
    };
    readMobileTransfer: {
        parameters: {
            query?: never;
            header: {
                /** @description This read request UUID */
                "Operation-Id": string;
                /** @description Selects a currently valid website session cookie */
                "Session-Id": string;
            };
            path: {
                /** @description Original transfer UUID */
                id: components["schemas"]["transfer.Id"];
            };
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            /** @description Pending, completed or expired transfer */
            200: {
                headers: {
                    /** @description no-store */
                    "Cache-Control"?: string;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["transfer.Status"];
                };
            };
            /** @description invalid_input; no query or body is accepted */
            400: {
                headers: {
                    /** @description no-store */
                    "Cache-Control"?: string;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Failure"];
                };
            };
            /** @description authentication_invalid */
            401: {
                headers: {
                    /** @description no-store */
                    "Cache-Control"?: string;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Failure"];
                };
            };
            /** @description permission_denied */
            403: {
                headers: {
                    /** @description no-store */
                    "Cache-Control"?: string;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Failure"];
                };
            };
            /** @description target_unavailable */
            404: {
                headers: {
                    /** @description no-store */
                    "Cache-Control"?: string;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Failure"];
                };
            };
            /** @description invalid_input */
            413: {
                headers: {
                    /** @description no-store */
                    "Cache-Control"?: string;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Failure"];
                };
            };
            /** @description rate_limited */
            429: {
                headers: {
                    /** @description no-store */
                    "Cache-Control"?: string;
                    /** @description Wait seconds */
                    "Retry-After"?: number;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Failure"];
                };
            };
            /** @description internal_error */
            500: {
                headers: {
                    /** @description no-store */
                    "Cache-Control"?: string;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Failure"];
                };
            };
            /** @description dependency_unavailable */
            503: {
                headers: {
                    /** @description no-store */
                    "Cache-Control"?: string;
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Failure"];
                };
            };
        };
    };
}
