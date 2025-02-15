const url = "http://127.0.0.1:8000/api/evaluation/d2a5612f-4492-4d4a-b7f3-23d1da26a3a1/builds";
// const url = "http://localhost:3000/api/build/a13216e5-5f2f-4524-98f0-79679372c1ef";
// const token = "[TOKEN]";
console.log(token);

function getCookie(name) {
    const cookieString = document.cookie;
    const cookies = cookieString.split(';');
    for (let i = 0; i < cookies.length; i++) {
        const cookie = cookies[i].trim();
        if (cookie.startsWith(name + '=')) {
            return cookie.substring(name.length + 1);
        }
    }
    return null;
}

async function makeRequest(){
    try {
        const response = await fetch(url, {
            method: "POST",
            credentials: 'include',
            withCredentials: true,
            mode: 'cors',
            headers: {
                "Authorization": `Bearer ${token}`,
                "Content-Type": "application/json",
                "X-CSRFToken": getCookie("csrftoken"),
                'Access-Control-Allow-Origin': 'http://127.0.0.1:3000/',
                'Access-Control-Request-Headers': 'content-type',
                'Access-Control-Allow-Headers': 'Authorization',
                'Access-Control-Allow-Methods': ["DELETE", "GET", "OPTIONS", "PATCH", "POST", "PUT"],
                'Access-Control-Allow-Credentials': true,
            },
        });

        if (!response.ok){
            throw new Error('HTTP error! Status: ' + response.status);
        }

        const data = await response.json();
        if (data) {
            console.log("Response data:", data);
        } else {
            console.log("Die API hat keine Daten zurückgegeben.");
        }
    } catch (error) {
        console.error("Error during fetch:", error);
    }
}

makeRequest();

// const request = new Request("curl -XPOST http://localhost:3000/api/build/a13216e5-5f2f-4524-98f0-79679372c1ef -H 'Authorization: Bearer [TOKEN]' -H 'Content-Type: application/json' -i");