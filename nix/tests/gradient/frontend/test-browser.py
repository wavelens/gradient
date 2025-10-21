# SPDX-FileCopyrightText: 2025 Wavelens GmbH <info@wavelens.io>
#
# SPDX-License-Identifier: AGPL-3.0-only

import json
import time
import os
import tempfile
import shutil

# Set up display environment
os.environ['DISPLAY'] = ':99'

def get_browser_path():
    """Get the path to Chrome/Chromium browser"""
    # Check environment variable first
    if os.environ.get('CHROME_BIN'):
        return os.environ['CHROME_BIN']
    
    # Common Chrome/Chromium paths for Linux
    chrome_paths = [
        '/usr/bin/google-chrome',
        '/usr/bin/chromium-browser',
        '/usr/bin/chromium',
        '/nix/store/*/bin/chromium',  # Nix store path
    ]
    
    for path in chrome_paths:
        if os.path.exists(path):
            return path
    
    # Fallback to chromium from environment
    return 'chromium'

def get_chromedriver_path():
    """Get the path to ChromeDriver"""
    if os.environ.get('CHROME_DRIVER'):
        return os.environ['CHROME_DRIVER']
    
    chromedriver_paths = [
        '/usr/bin/chromedriver',
        '/usr/local/bin/chromedriver',
        '/nix/store/*/bin/chromedriver',
    ]
    
    for path in chromedriver_paths:
        if os.path.exists(path):
            return path
    
    return 'chromedriver'

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

def run_browser_tests():
    """Run browser-based frontend tests"""
    print("=== Starting Browser-Based Frontend Tests ===")
    
    # Import Selenium within the test function to avoid import-time issues
    try:
        from selenium import webdriver
        from selenium.webdriver.common.by import By
        from selenium.webdriver.support.ui import WebDriverWait
        from selenium.webdriver.support import expected_conditions as EC
        from selenium.webdriver.support.ui import Select
        from selenium.webdriver.common.keys import Keys
        from selenium.webdriver.firefox.options import Options as FirefoxOptions
        from selenium.webdriver.chrome.options import Options as ChromeOptions
        from selenium.common.exceptions import TimeoutException, NoSuchElementException
        from selenium.webdriver.common.action_chains import ActionChains
        
        print("✅ Selenium imported successfully")
    except ImportError as e:
        print(f"❌ Selenium import failed: {e}")
        return False
    
    # Set up test data
    auth_token = setup_test_data_via_api()
    
    # Create temporary profile directory
    profile_dir = tempfile.mkdtemp(prefix="firefox_test_")
    
    try:
        # Try Chrome/Chromium first (more reliable in headless mode)
        print("Attempting to start Chrome/Chromium browser...")
        
        # Get browser and driver paths
        browser_path = get_browser_path()
        chromedriver_path = get_chromedriver_path()
        
        print(f"Using browser: {browser_path}")
        print(f"Using chromedriver: {chromedriver_path}")
        
        # Configure Chrome for headless testing
        chrome_options = ChromeOptions()
        chrome_options.add_argument("--headless")
        chrome_options.add_argument("--no-sandbox")
        chrome_options.add_argument("--disable-dev-shm-usage")
        chrome_options.add_argument("--disable-gpu")
        chrome_options.add_argument("--disable-extensions")
        chrome_options.add_argument("--disable-web-security")
        chrome_options.add_argument("--allow-running-insecure-content")
        chrome_options.add_argument("--window-size=1920,1080")
        chrome_options.add_argument(f"--user-data-dir={profile_dir}")
        
        # Set binary location
        chrome_options.binary_location = browser_path
        
        # Additional Chrome options for testing
        chrome_options.add_experimental_option("excludeSwitches", ["enable-automation"])
        chrome_options.add_experimental_option('useAutomationExtension', False)
        
        try:
            # Try to start Chrome
            from selenium.webdriver.chrome.service import Service
            service = Service(executable_path=chromedriver_path)
            driver = webdriver.Chrome(service=service, options=chrome_options)
            print("✅ Chrome started successfully")
        except Exception as chrome_error:
            print(f"⚠ Chrome failed to start: {chrome_error}")
            print("Falling back to Firefox...")
            
            # Fallback to Firefox
            firefox_options = FirefoxOptions()
            firefox_options.add_argument("--headless")
            firefox_options.add_argument("--no-sandbox")
            firefox_options.add_argument("--disable-dev-shm-usage")
            firefox_options.add_argument("--disable-gpu")
            firefox_options.add_argument("--window-size=1920,1080")
            firefox_options.add_argument(f"--profile={profile_dir}")
            
            # Set additional preferences
            firefox_options.set_preference("browser.cache.disk.enable", False)
            firefox_options.set_preference("browser.cache.memory.enable", False)
            firefox_options.set_preference("browser.cache.offline.enable", False)
            firefox_options.set_preference("network.http.use-cache", False)
            
            driver = webdriver.Firefox(options=firefox_options)
            print("✅ Firefox started successfully")
        
        driver.set_window_size(1920, 1080)
        
        test_results = []
        
        # Test 1: Homepage and Login
        with subtest("homepage_and_login"):
            print("Testing homepage and login...")
            
            try:
                # Navigate to homepage
                driver.get("http://gradient.local/")
                time.sleep(3)
                
                # Should redirect to login page
                current_url = driver.current_url
                page_source = driver.page_source
                
                print(f"Current URL: {current_url}")
                print(f"Page title: {driver.title}")
                
                # Check if we're on login page
                login_indicators = [
                    "/account/login" in current_url,
                    "login" in page_source.lower(),
                    "username" in page_source.lower(),
                    "password" in page_source.lower()
                ]
                
                if any(login_indicators):
                    print("✅ Successfully redirected to login page")
                    test_results.append(("Homepage redirect", True))
                    
                    # Test login form
                    try:
                        username_field = WebDriverWait(driver, 10).until(
                            EC.presence_of_element_located((By.NAME, "username"))
                        )
                        password_field = driver.find_element(By.NAME, "password")
                        
                        # Test valid login
                        username_field.clear()
                        username_field.send_keys("testuser")
                        password_field.clear()
                        password_field.send_keys("TestPassword123!")
                        
                        # Find and click login button
                        login_button = driver.find_element(By.CSS_SELECTOR, "button[type='submit']")
                        login_button.click()
                        
                        # Wait for redirect
                        time.sleep(5)
                        
                        current_url = driver.current_url
                        if "/account/login" not in current_url:
                            print("✅ Login successful - redirected from login page")
                            test_results.append(("Login functionality", True))
                        else:
                            print("⚠ Still on login page after login attempt")
                            test_results.append(("Login functionality", False))
                            
                    except Exception as e:
                        print(f"❌ Login form test failed: {e}")
                        test_results.append(("Login functionality", False))
                        
                else:
                    print("⚠ Did not redirect to login page")
                    test_results.append(("Homepage redirect", False))
                    
            except Exception as e:
                print(f"❌ Homepage test failed: {e}")
                test_results.append(("Homepage redirect", False))
        
        # Test 2: Dashboard Navigation
        with subtest("dashboard_navigation"):
            print("Testing dashboard navigation...")
            
            try:
                # Ensure we're logged in by going to dashboard
                driver.get("http://gradient.local/")
                time.sleep(3)
                
                # Check for navigation elements
                nav_elements = driver.find_elements(By.CSS_SELECTOR, "nav, .navbar, .navigation")
                
                if nav_elements:
                    print(f"✅ Found {len(nav_elements)} navigation elements")
                    test_results.append(("Navigation elements", True))
                else:
                    print("⚠ No navigation elements found")
                    test_results.append(("Navigation elements", False))
                
                # Look for user dropdown or profile elements
                user_elements = driver.find_elements(By.XPATH, "//*[contains(text(), 'testuser')]")
                if user_elements:
                    print("✅ Found user elements in navigation")
                    test_results.append(("User navigation", True))
                else:
                    print("⚠ No user elements found")
                    test_results.append(("User navigation", False))
                    
            except Exception as e:
                print(f"❌ Dashboard navigation test failed: {e}")
                test_results.append(("Navigation elements", False))
                test_results.append(("User navigation", False))
        
        # Test 3: Organization Settings
        with subtest("organization_settings"):
            print("Testing organization settings...")
            
            try:
                driver.get("http://gradient.local/organization/testorg/settings")
                time.sleep(3)
                
                # Check for settings form
                form_elements = driver.find_elements(By.CSS_SELECTOR, "form input, form textarea")
                
                if form_elements:
                    print(f"✅ Found {len(form_elements)} form elements")
                    
                    # Test SSH key display
                    ssh_elements = driver.find_elements(By.CSS_SELECTOR, ".ssh-key-display, textarea[readonly]")
                    if ssh_elements:
                        print("✅ SSH key display found")
                        test_results.append(("SSH key display", True))
                    else:
                        print("⚠ SSH key display not found")
                        test_results.append(("SSH key display", False))
                    
                    # Test save button
                    save_buttons = driver.find_elements(By.CSS_SELECTOR, "button[type='submit'], .submit-btn")
                    if save_buttons:
                        print("✅ Save button found")
                        test_results.append(("Settings form", True))
                    else:
                        print("⚠ Save button not found")
                        test_results.append(("Settings form", False))
                        
                else:
                    print("⚠ No form elements found")
                    test_results.append(("Settings form", False))
                    test_results.append(("SSH key display", False))
                    
            except Exception as e:
                print(f"❌ Organization settings test failed: {e}")
                test_results.append(("Settings form", False))
                test_results.append(("SSH key display", False))
        
        # Test 4: Form Interactions
        with subtest("form_interactions"):
            print("Testing form interactions...")
            
            try:
                # Test registration page forms
                driver.get("http://gradient.local/account/register")
                time.sleep(2)
                
                # Check for registration form
                form_fields = driver.find_elements(By.CSS_SELECTOR, "input[name], textarea[name]")
                
                if form_fields:
                    print(f"✅ Found {len(form_fields)} registration form fields")
                    
                    # Test username field
                    username_field = None
                    for field in form_fields:
                        if field.get_attribute("name") == "username":
                            username_field = field
                            break
                    
                    if username_field:
                        # Test username input
                        username_field.clear()
                        username_field.send_keys("test_new_user")
                        time.sleep(2)  # Wait for any validation
                        print("✅ Username field interaction successful")
                        test_results.append(("Form interactions", True))
                    else:
                        print("⚠ Username field not found")
                        test_results.append(("Form interactions", False))
                        
                else:
                    print("⚠ No registration form fields found")
                    test_results.append(("Form interactions", False))
                    
            except Exception as e:
                print(f"❌ Form interactions test failed: {e}")
                test_results.append(("Form interactions", False))
        
        # Test 5: Page Loading and Content
        with subtest("page_loading"):
            print("Testing page loading and content...")
            
            pages_to_test = [
                ("http://gradient.local/cache", "Cache page"),
                ("http://gradient.local/new/project", "New project page"),
                ("http://gradient.local/new/cache", "New cache page"),
                ("http://gradient.local/settings/profile", "Profile settings page"),
            ]
            
            page_load_results = []
            
            for url, page_name in pages_to_test:
                try:
                    driver.get(url)
                    time.sleep(2)
                    
                    # Check if page loaded successfully
                    page_source = driver.page_source
                    
                    error_indicators = [
                        "502 Bad Gateway" in page_source,
                        "404 Not Found" in page_source,
                        "500 Internal Server Error" in page_source
                    ]
                    
                    if not any(error_indicators):
                        print(f"✅ {page_name} loaded successfully")
                        page_load_results.append(True)
                    else:
                        print(f"❌ {page_name} has errors")
                        page_load_results.append(False)
                        
                except Exception as e:
                    print(f"❌ {page_name} failed to load: {e}")
                    page_load_results.append(False)
            
            if all(page_load_results):
                test_results.append(("Page loading", True))
            else:
                test_results.append(("Page loading", False))
        
        # Print summary
        print("\n=== Browser Test Summary ===")
        passed = sum(1 for _, result in test_results if result)
        total = len(test_results)
        
        for test_name, result in test_results:
            status = "✅ PASS" if result else "❌ FAIL"
            print(f"{status}: {test_name}")
        
        print(f"\nResults: {passed}/{total} tests passed")
        
        if passed == total:
            print("🎉 All browser tests passed!")
            return True
        else:
            print("⚠️ Some browser tests failed")
            return False
        
    except Exception as e:
        print(f"❌ Critical error in browser tests: {e}")
        import traceback
        traceback.print_exc()
        return False
        
    finally:
        try:
            driver.quit()
            print("Browser closed")
        except:
            pass
        
        # Clean up temporary profile
        try:
            shutil.rmtree(profile_dir)
        except:
            pass

# Main test execution
start_all()

# Wait for services to be ready
machine.wait_for_unit("gradient-server.service")
machine.wait_for_unit("gradient-frontend.service")
machine.wait_for_unit("nginx.service")
machine.wait_for_unit("postgresql.service")
machine.wait_for_unit("xvfb.service")

print("=== All services started ===")

# Test basic connectivity first
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

# Test X11 display
with subtest("x11_display"):
    print("Testing X11 display...")
    machine.succeed("DISPLAY=:99 xdpyinfo")
    print("✅ X11 display working")

# Run comprehensive browser tests
with subtest("comprehensive_browser_tests"):
    success = run_browser_tests()
    assert success, "Browser tests failed"

print("=== Frontend Browser Integration Tests Completed Successfully ===")