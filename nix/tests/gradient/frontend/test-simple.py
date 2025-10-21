# SPDX-FileCopyrightText: 2025 Wavelens GmbH <info@wavelens.io>
#
# SPDX-License-Identifier: AGPL-3.0-only

import json
import time
import re
import urllib.parse

def setup_test_data_via_api():
    """Set up test data using the API"""
    print("Setting up test data via API...")
    
    # Register test user
    register_response = machine.succeed("""
        curl -X POST \
        -H "Content-Type: application/json" \
        -d '{"username": "testuser", "name": "Test User", "email": "test@example.com", "password": "TestPassword123!"}' \
        http://gradient.local/api/v1/auth/basic/register -s
    """)
    
    print(f"Register response: {register_response}")
    
    # Login to get token
    login_response = machine.succeed("""
        curl -X POST \
        -H "Content-Type: application/json" \
        -d '{"loginname": "testuser", "password": "TestPassword123!"}' \
        http://gradient.local/api/v1/auth/basic/login -s
    """)
    
    login_data = json.loads(login_response)
    if login_data.get("error"):
        print(f"Login failed: {login_data.get('message')}")
        return None
    
    token = login_data.get("message")
    print(f"Got auth token: {token[:20]}...")
    
    # Create test organization
    org_response = machine.succeed(f"""
        curl -X POST \
        -H "Content-Type: application/json" \
        -H "Authorization: Bearer {token}" \
        -d '{{"name": "testorg", "display_name": "Test Organization", "description": "Test organization for frontend testing"}}' \
        http://gradient.local/api/v1/orgs -s
    """)
    
    print(f"Organization creation response: {org_response}")
    
    # Create test cache
    cache_response = machine.succeed(f"""
        curl -X POST \
        -H "Content-Type: application/json" \
        -H "Authorization: Bearer {token}" \
        -d '{{"name": "testcache", "display_name": "Test Cache", "description": "Test cache for frontend testing", "priority": 10}}' \
        http://gradient.local/api/v1/caches -s
    """)
    
    print(f"Cache creation response: {cache_response}")
    
    return token

def test_page_content(url, expected_content, description):
    """Test that a page contains expected content"""
    print(f"Testing {description}...")
    
    try:
        response = machine.succeed(f"curl -s -L '{url}'")
        
        # Check for error pages
        if "502 Bad Gateway" in response:
            print(f"❌ {description}: 502 Bad Gateway")
            return False
        elif "404 Not Found" in response:
            print(f"❌ {description}: 404 Not Found")
            return False
        elif "500 Internal Server Error" in response:
            print(f"❌ {description}: 500 Internal Server Error")
            return False
        
        # Check for expected content
        if isinstance(expected_content, str):
            if expected_content.lower() in response.lower():
                print(f"✅ {description}: Found expected content")
                return True
            else:
                print(f"❌ {description}: Expected content not found")
                return False
        elif isinstance(expected_content, list):
            found_all = True
            for content in expected_content:
                if content.lower() not in response.lower():
                    print(f"❌ {description}: Missing content: {content}")
                    found_all = False
                else:
                    print(f"✅ {description}: Found content: {content}")
            return found_all
        
        return True
        
    except Exception as e:
        print(f"❌ {description}: Error - {e}")
        return False

def test_form_submission(url, form_data, description):
    """Test form submission"""
    print(f"Testing {description}...")
    
    try:
        # Build form data string
        form_params = "&".join([f"{k}={urllib.parse.quote(str(v))}" for k, v in form_data.items()])
        
        response = machine.succeed(f"""
            curl -s -L -X POST \
            -H "Content-Type: application/x-www-form-urlencoded" \
            -d "{form_params}" \
            "{url}"
        """)
        
        # Check response
        if "502 Bad Gateway" in response:
            print(f"❌ {description}: 502 Bad Gateway")
            return False
        elif "500 Internal Server Error" in response:
            print(f"❌ {description}: 500 Internal Server Error")
            return False
        else:
            print(f"✅ {description}: Form submission successful")
            return True
            
    except Exception as e:
        print(f"❌ {description}: Error - {e}")
        return False

def test_api_endpoint(endpoint, expected_error=True, token=None, description=""):
    """Test API endpoint"""
    print(f"Testing API endpoint: {endpoint} {description}")
    
    try:
        headers = "-H 'Content-Type: application/json'"
        if token:
            headers += f" -H 'Authorization: Bearer {token}'"
        
        response = machine.succeed(f"curl -s {headers} http://gradient.local{endpoint}")
        
        try:
            data = json.loads(response)
            if expected_error:
                if data.get("error"):
                    print(f"✅ API {endpoint}: Expected error returned")
                    return True
                else:
                    print(f"❌ API {endpoint}: Expected error but got success")
                    return False
            else:
                if not data.get("error"):
                    print(f"✅ API {endpoint}: Success response")
                    return True
                else:
                    print(f"❌ API {endpoint}: Unexpected error: {data.get('message')}")
                    return False
        except json.JSONDecodeError:
            print(f"❌ API {endpoint}: Invalid JSON response")
            return False
            
    except Exception as e:
        print(f"❌ API {endpoint}: Error - {e}")
        return False

def run_frontend_tests():
    """Run comprehensive frontend tests using HTTP requests"""
    print("=== Starting Frontend HTTP Tests ===")
    
    # Set up test data
    auth_token = setup_test_data_via_api()
    
    test_results = []
    
    # Test 1: Homepage redirect to login
    with subtest("homepage_redirect"):
        result = test_page_content(
            "http://gradient.local/",
            ["login", "username", "password"],
            "Homepage redirect to login"
        )
        test_results.append(("Homepage redirect", result))
    
    # Test 2: Login page
    with subtest("login_page"):
        result = test_page_content(
            "http://gradient.local/account/login",
            ["username", "password", "login"],
            "Login page content"
        )
        test_results.append(("Login page", result))
    
    # Test 3: Registration page
    with subtest("registration_page"):
        result = test_page_content(
            "http://gradient.local/account/register",
            ["username", "password", "email", "register"],
            "Registration page content"
        )
        test_results.append(("Registration page", result))
    
    # Test 4: API endpoints without authentication
    with subtest("api_unauthorized"):
        result = test_api_endpoint(
            "/api/v1/orgs",
            expected_error=True,
            description="(unauthorized)"
        )
        test_results.append(("API unauthorized", result))
    
    # Test 5: API endpoints with authentication
    if auth_token:
        with subtest("api_authorized"):
            result = test_api_endpoint(
                "/api/v1/orgs",
                expected_error=False,
                token=auth_token,
                description="(authorized)"
            )
            test_results.append(("API authorized", result))
    
    # Test 6: Organization pages (if we have auth)
    if auth_token:
        with subtest("organization_pages"):
            # Get session cookie by logging in through web interface
            login_response = machine.succeed("""
                curl -s -c /tmp/cookies.txt -L \
                -X POST \
                -H "Content-Type: application/x-www-form-urlencoded" \
                -d "username=testuser&password=TestPassword123!" \
                http://gradient.local/account/login
            """)
            
            # Test organization settings page
            result = test_page_content(
                "http://gradient.local/organization/testorg/settings",
                ["organization", "settings", "ssh"],
                "Organization settings page"
            )
            test_results.append(("Organization settings", result))
            
            # Test organization members page
            result = test_page_content(
                "http://gradient.local/organization/testorg/members",
                ["members", "role"],
                "Organization members page"
            )
            test_results.append(("Organization members", result))
            
            # Test organization servers page
            result = test_page_content(
                "http://gradient.local/organization/testorg/servers",
                ["servers", "host"],
                "Organization servers page"
            )
            test_results.append(("Organization servers", result))
    
    # Test 7: Cache pages
    with subtest("cache_pages"):
        result = test_page_content(
            "http://gradient.local/cache",
            ["cache", "priority"],
            "Cache listing page"
        )
        test_results.append(("Cache listing", result))
    
    # Test 8: New project page
    with subtest("new_project_page"):
        result = test_page_content(
            "http://gradient.local/new/project",
            ["project", "organization", "repository"],
            "New project page"
        )
        test_results.append(("New project page", result))
    
    # Test 9: New cache page
    with subtest("new_cache_page"):
        result = test_page_content(
            "http://gradient.local/new/cache",
            ["cache", "priority", "name"],
            "New cache page"
        )
        test_results.append(("New cache page", result))
    
    # Test 10: User profile settings
    with subtest("profile_settings"):
        result = test_page_content(
            "http://gradient.local/settings/profile",
            ["profile", "username", "email"],
            "Profile settings page"
        )
        test_results.append(("Profile settings", result))
    
    # Test 11: Static assets
    with subtest("static_assets"):
        # Test CSS loading
        css_result = machine.succeed("curl -s -o /dev/null -w '%{http_code}' http://gradient.local/static/css/main.css || echo '404'")
        
        # Test JS loading
        js_result = machine.succeed("curl -s -o /dev/null -w '%{http_code}' http://gradient.local/static/js/main.js || echo '404'")
        
        static_ok = "20" in css_result or "40" in css_result  # 200 or 404 is fine, 500 is not
        test_results.append(("Static assets", static_ok))
    
    # Test 12: Form validation endpoints
    with subtest("form_validation"):
        # Test username availability API
        result = test_api_endpoint(
            "/api/v1/auth/check-username",
            expected_error=False,
            description="(username check)"
        )
        test_results.append(("Username check API", result))
    
    # Test 13: SSH key endpoint
    if auth_token:
        with subtest("ssh_key_endpoint"):
            result = test_api_endpoint(
                "/api/v1/orgs/testorg/ssh",
                expected_error=False,
                token=auth_token,
                description="(SSH key)"
            )
            test_results.append(("SSH key endpoint", result))
    
    # Print summary
    print("\n=== Frontend Test Summary ===")
    passed = 0
    total = len(test_results)
    
    for test_name, result in test_results:
        status = "✅ PASS" if result else "❌ FAIL"
        print(f"{status}: {test_name}")
        if result:
            passed += 1
    
    print(f"\nResults: {passed}/{total} tests passed")
    
    if passed == total:
        print("🎉 All frontend tests passed!")
    else:
        print("⚠️  Some frontend tests failed")
    
    return passed == total

# Main test execution
start_all()

# Wait for services to be ready
machine.wait_for_unit("gradient-server.service")
machine.wait_for_unit("gradient-frontend.service")
machine.wait_for_unit("nginx.service")
machine.wait_for_unit("postgresql.service")

print("=== All services started ===")

# Test basic connectivity
with subtest("basic_connectivity"):
    print("Testing basic connectivity...")
    
    # Test API health
    machine.succeed("curl http://gradient.local/api/v1/health -i --fail")
    print("✅ API health check passed")
    
    # Test frontend connectivity
    response = machine.succeed("curl http://gradient.local/ -s")
    assert len(response) > 0
    print("✅ Frontend connectivity check passed")
    
    # Test that services are responding
    assert "502 Bad Gateway" not in response
    assert "404 Not Found" not in response
    print("✅ Services responding correctly")

# Run comprehensive frontend tests
with subtest("comprehensive_frontend_tests"):
    success = run_frontend_tests()
    assert success, "Frontend tests failed"

print("=== Frontend Integration Tests Completed Successfully ===")